#![allow(clippy::mutable_key_type)]

use std::collections::BTreeMap;
use std::vec::Drain;

use bytes::Bytes;
use risingwave_common::array::{Row, RowDeserializer};
use risingwave_common::error::Result;
use risingwave_common::types::DataTypeKind;
use risingwave_storage::{Keyspace, StateStore};

use crate::stream_op::managed_state::aggregation::OrderedRowDeserializer;
use crate::stream_op::managed_state::flush_status::FlushStatus;
use crate::stream_op::managed_state::OrderedRow;
use crate::stream_op::serialize_cell_idx;

/// This state is used for `[offset...offset+limit)` part in the `TopNExecutor`.
///
/// Since the elements in this range may be moved to `[0..offset)` or `[offset+limit..+inf)`,
/// we would like to cache the two ends of the range. Since this would call for a `reverse iterator`
/// from `Hummock`, we temporarily adopt a all-or-nothing cache policy instead of a top-n and a
/// bottom-n policy.
pub struct ManagedTopNBottomNState<S: StateStore> {
    /// Top-N Cache.
    top_n: BTreeMap<OrderedRow, Row>,
    /// Bottom-N Cache. We always try to first fill into the bottom-n cache.
    bottom_n: BTreeMap<OrderedRow, Row>,
    /// Buffer for updates.
    flush_buffer: BTreeMap<OrderedRow, FlushStatus<Row>>,
    /// The number of elements in both cache and storage.
    total_count: usize,
    /// Number of entries to retain in top-n cache after each flush.
    top_n_count: Option<usize>,
    /// Number of entries to retain in bottom-n cache after each flush.
    bottom_n_count: Option<usize>,
    /// The keyspace to operate on.
    keyspace: Keyspace<S>,
    /// `DataTypeKind`s use for deserializing `Row`.
    data_type_kinds: Vec<DataTypeKind>,
    /// For deserializing `OrderedRow`.
    ordered_row_deserializer: OrderedRowDeserializer,
}

impl<S: StateStore> ManagedTopNBottomNState<S> {
    pub fn new(
        cache_size: Option<usize>,
        total_count: usize,
        keyspace: Keyspace<S>,
        data_type_kinds: Vec<DataTypeKind>,
        ordered_row_deserializer: OrderedRowDeserializer,
    ) -> Self {
        Self {
            top_n: BTreeMap::new(),
            bottom_n: BTreeMap::new(),
            flush_buffer: BTreeMap::new(),
            total_count,
            top_n_count: cache_size,
            bottom_n_count: cache_size,
            keyspace,
            data_type_kinds,
            ordered_row_deserializer,
        }
    }

    pub fn total_count(&self) -> usize {
        self.total_count
    }

    pub fn is_dirty(&self) -> bool {
        !self.flush_buffer.is_empty()
    }

    // May have weird cache policy in the future, reserve an `n`.
    pub fn retain_top_n(&mut self, n: usize) {
        while self.top_n.len() > n {
            self.top_n.pop_first();
        }
    }

    // May have weird cache policy in the future, reserve an `n`.
    pub fn retain_bottom_n(&mut self, n: usize) {
        while self.bottom_n.len() > n {
            self.bottom_n.pop_last();
        }
    }

    pub fn retain_both_n(&mut self) {
        if let Some(n) = self.top_n_count {
            self.retain_top_n(n);
        }
        if let Some(n) = self.bottom_n_count {
            self.retain_bottom_n(n);
        }
    }

    pub async fn pop_top_element(&mut self) -> Result<Option<(OrderedRow, Row)>> {
        if self.total_count == 0 {
            Ok(None)
        } else {
            let cache_to_pop = if self.top_n.is_empty() {
                &self.bottom_n
            } else {
                &self.top_n
            };
            let key = cache_to_pop.last_key_value().unwrap().0.clone();
            let value = self.delete(&key).await?;
            Ok(Some((key, value.unwrap())))
        }
    }

    pub async fn pop_bottom_element(&mut self) -> Result<Option<(OrderedRow, Row)>> {
        if self.total_count == 0 {
            Ok(None)
        } else {
            let cache_to_pop = if self.bottom_n.is_empty() {
                &self.top_n
            } else {
                &self.bottom_n
            };
            let key = cache_to_pop.first_key_value().unwrap().0.clone();
            let value = self.delete(&key).await?;
            Ok(Some((key, value.unwrap())))
        }
    }

    pub fn top_element(&mut self) -> Option<(&OrderedRow, &Row)> {
        if self.total_count == 0 {
            None
        } else if self.top_n.is_empty() {
            self.bottom_n.last_key_value()
        } else {
            self.top_n.last_key_value()
        }
    }

    pub fn bottom_element(&mut self) -> Option<(&OrderedRow, &Row)> {
        if self.total_count == 0 {
            None
        } else if self.bottom_n.is_empty() {
            self.top_n.first_key_value()
        } else {
            self.bottom_n.first_key_value()
        }
    }

    pub async fn insert(&mut self, key: OrderedRow, value: Row) {
        // We can have different strategy of which cache we should insert the element into.
        // Right now, we keep it simple and insert the element into the cache with smaller size,
        // without violating the constraint that these two caches' current range must NOT overlap.
        let top_n_size = self.top_n.len();
        let bottom_n_size = self.bottom_n.len();
        let insert_to_cache = if top_n_size > bottom_n_size {
            // Since `>`, directly `unwrap`
            if self.top_n.first_key_value().unwrap().0 < &key {
                &mut self.top_n
            } else {
                &mut self.bottom_n
            }
        } else if self.bottom_n.is_empty() || self.bottom_n.last_key_value().unwrap().0 <= &key {
            &mut self.top_n
        } else {
            &mut self.bottom_n
        };
        insert_to_cache.insert(key.clone(), value.clone());
        FlushStatus::do_insert(self.flush_buffer.entry(key), value);
        self.total_count += 1;
    }

    pub async fn delete(&mut self, key: &OrderedRow) -> Result<Option<Row>> {
        let prev_top_n_entry = self.top_n.remove(key);
        let prev_bottom_n_entry = self.bottom_n.remove(key);
        debug_assert!(prev_top_n_entry.is_some() || prev_bottom_n_entry.is_some());
        FlushStatus::do_delete(self.flush_buffer.entry(key.clone()));
        self.total_count -= 1;
        // If we have nothing in both caches, we have to scan from the storage.
        if self.top_n.is_empty() && self.bottom_n.is_empty() && self.total_count > 0 {
            self.scan_and_merge().await?;
        }
        Ok(prev_top_n_entry)
    }

    /// The same as the one in `ManagedTopNState`.
    pub async fn scan_and_merge(&mut self) -> Result<()> {
        let mut kv_pairs = self.scan_from_storage(None).await?;
        let mut flush_buffer_iter = self.flush_buffer.iter().peekable();
        let mut insert_process =
            |cache: &mut BTreeMap<OrderedRow, Row>, part_kv_pairs: Drain<(OrderedRow, Row)>| {
                for (key_from_storage, row_from_storage) in part_kv_pairs {
                    while let Some((key_from_buffer, _)) = flush_buffer_iter.peek() {
                        if **key_from_buffer >= key_from_storage {
                            break;
                        } else {
                            flush_buffer_iter.next();
                        }
                    }
                    if flush_buffer_iter.peek().is_none() {
                        cache.insert(key_from_storage, row_from_storage);
                        continue;
                    }
                    let (key_from_buffer, value_from_buffer) = flush_buffer_iter.peek().unwrap();
                    match key_from_storage.cmp(key_from_buffer) {
                        std::cmp::Ordering::Equal => {
                            match value_from_buffer {
                                FlushStatus::Delete => {
                                    // do not put it into cache
                                }
                                FlushStatus::Insert(row) | FlushStatus::DeleteInsert(row) => {
                                    cache.insert(key_from_storage, row.clone());
                                }
                            }
                        }
                        std::cmp::Ordering::Greater => {
                            flush_buffer_iter.next();
                        }
                        _ => unreachable!(),
                    }
                }
            };
        // The reason we can split the `kv_pairs` without caring whether the key to be inserted is
        // already in the top_n or bottom_n is that we would only trigger `scan_and_merge` when both
        // caches are empty.
        {
            let part1 = kv_pairs.drain(0..kv_pairs.len() / 2);
            insert_process(&mut self.bottom_n, part1);
        }
        {
            let part2 = kv_pairs.drain(..);
            insert_process(&mut self.top_n, part2);
        }
        Ok(())
    }

    async fn scan_from_storage(
        &mut self,
        number_rows: Option<usize>,
    ) -> Result<Vec<(OrderedRow, Row)>> {
        let pk_row_bytes = self
            .keyspace
            .scan_strip_prefix(
                number_rows.map(|top_n_count| top_n_count * self.data_type_kinds.len()),
            )
            .await?;
        // We must have enough cells to restore a complete row.
        debug_assert_eq!(pk_row_bytes.len() % self.data_type_kinds.len(), 0);
        // cell-based storage format, so `self.schema.len()`
        let mut row_bytes = vec![];
        let mut cell_restored = 0;
        let mut res = vec![];
        for (pk, cell_bytes) in pk_row_bytes {
            row_bytes.extend_from_slice(&cell_bytes);
            cell_restored += 1;
            if cell_restored == self.data_type_kinds.len() {
                cell_restored = 0;
                let deserializer = RowDeserializer::new(self.data_type_kinds.clone());
                let row = deserializer.deserialize(&std::mem::take(&mut row_bytes))?;
                // format: [pk_buf | cell_idx (4B)]
                // Take `pk_buf` out.
                let pk_without_cell_idx = pk.slice(0..pk.len() - 4);
                let ordered_row = self
                    .ordered_row_deserializer
                    .deserialize(&pk_without_cell_idx)?;
                res.push((ordered_row, row));
            }
        }
        Ok(res)
    }

    /// We can fill in the cache from storage only when state is not dirty, i.e. right after
    /// `flush`.
    pub async fn fill_in_cache(&mut self) -> Result<()> {
        debug_assert!(!self.is_dirty());
        let mut pk_row_bytes = self.scan_from_storage(None).await?;
        // cell-based storage format, so `self.schema.len()`
        for (pk, row) in pk_row_bytes.drain(0..pk_row_bytes.len() / 2) {
            self.bottom_n.insert(pk, row);
        }
        for (pk, row) in pk_row_bytes.drain(..) {
            self.top_n.insert(pk, row);
        }
        // We don't retain `n` elements as we have a all-or-nothing policy for now.
        Ok(())
    }

    /// `Flush` can be called by the executor when it receives a barrier and thus needs to
    /// checkpoint.
    pub async fn flush(&mut self) -> Result<()> {
        if !self.is_dirty() {
            // We don't retain `n` elements as we have a all-or-nothing policy for now.
            return Ok(());
        }

        let mut write_batches: Vec<(Bytes, Option<Bytes>)> = vec![];
        for (ordered_row, cells) in std::mem::take(&mut self.flush_buffer) {
            let row_option = cells.into_option();
            for cell_idx in 0..self.data_type_kinds.len() {
                // format: [pk_buf | cell_idx (4B)]
                let ordered_row_bytes = ordered_row.serialize()?;
                let key_encoded = [
                    &ordered_row_bytes[..],
                    &serialize_cell_idx(cell_idx as u32)?[..],
                ]
                .concat();
                // format: [keyspace prefix | pk_buf | cell_idx (4B)]
                let key_encoded = self.keyspace.prefixed_key(&key_encoded).into();
                match &row_option {
                    Some(row) => {
                        let row_bytes = row.serialize()?;
                        write_batches.push((key_encoded, Some(row_bytes.into())));
                    }
                    None => {
                        write_batches.push((key_encoded, None));
                    }
                };
            }
        }
        self.keyspace
            .state_store()
            .ingest_batch(write_batches)
            .await?;

        // We don't retain `n` elements as we have a all-or-nothing policy for now.
        Ok(())
    }
}

/// Test-related methods
impl<S: StateStore> ManagedTopNBottomNState<S> {
    #[cfg(test)]
    fn get_cache_len(&self) -> usize {
        self.top_n.len() + self.bottom_n.len()
    }

    #[cfg(test)]
    fn print_cached_elements(&self) {
        self.bottom_n
            .iter()
            .for_each(|(key, val)| println!("{:?}", (key, val)));
        println!("bottom_n above\ntop_n below\n");
        self.top_n
            .iter()
            .for_each(|(key, val)| println!("{:?}", (key, val)));
    }
}

#[cfg(test)]
mod tests {

    use risingwave_common::types::{DataType, DataTypeKind, Int64Type, StringType};
    use risingwave_common::util::sort_util::OrderType;
    use risingwave_storage::memory::MemoryStateStore;
    use risingwave_storage::{Keyspace, StateStore};

    use crate::row_nonnull;
    use crate::stream_op::managed_state::aggregation::OrderedRowDeserializer;
    use crate::stream_op::managed_state::top_n::top_n_bottom_n_state::ManagedTopNBottomNState;
    use crate::stream_op::managed_state::OrderedRow;

    fn create_managed_top_n_bottom_n_state<S: StateStore>(
        store: &S,
        row_count: usize,
        data_type_kinds: Vec<DataTypeKind>,
        order_types: Vec<OrderType>,
    ) -> ManagedTopNBottomNState<S> {
        let ordered_row_deserializer =
            OrderedRowDeserializer::new(data_type_kinds.clone(), order_types);

        ManagedTopNBottomNState::new(
            Some(1),
            row_count,
            Keyspace::executor_root(store.clone(), 0x2333),
            data_type_kinds,
            ordered_row_deserializer,
        )
    }

    #[tokio::test]
    async fn test_managed_top_n_bottom_n_state() {
        let data_type_kinds = vec![
            StringType::create(false, 5, DataTypeKind::Varchar).data_type_kind(),
            Int64Type::new(false).data_type_kind(),
        ];
        let order_types = vec![OrderType::Descending, OrderType::Ascending];
        let store = MemoryStateStore::new();
        let mut managed_state = create_managed_top_n_bottom_n_state(
            &store,
            0,
            data_type_kinds.clone(),
            order_types.clone(),
        );
        let row1 = row_nonnull!["abc".to_string(), 2i64];
        let row2 = row_nonnull!["abc".to_string(), 3i64];
        let row3 = row_nonnull!["abd".to_string(), 3i64];
        let row4 = row_nonnull!["ab".to_string(), 4i64];
        let rows = vec![row1, row2, row3, row4];
        let ordered_rows = rows
            .clone()
            .into_iter()
            .map(|row| OrderedRow::new(row, &order_types))
            .collect::<Vec<_>>();

        managed_state
            .insert(ordered_rows[3].clone(), rows[3].clone())
            .await;
        // now ("ab", 4)

        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[3], &rows[3]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[3], &rows[3]))
        );
        assert!(managed_state.is_dirty());
        assert_eq!(managed_state.get_cache_len(), 1);

        managed_state
            .insert(ordered_rows[2].clone(), rows[2].clone())
            .await;
        // now ("abd", 3) -> ("ab", 4)

        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[3], &rows[3]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        assert!(managed_state.is_dirty());
        assert_eq!(managed_state.get_cache_len(), 2);

        managed_state
            .insert(ordered_rows[1].clone(), rows[1].clone())
            .await;
        // now ("abd", 3) -> ("abc", 3) -> ("ab", 4)

        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[3], &rows[3]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        assert_eq!(managed_state.get_cache_len(), 3);
        managed_state.flush().await.unwrap();
        assert!(!managed_state.is_dirty());
        let row_count = managed_state.total_count;
        assert_eq!(row_count, 3);
        // After flush, all elements should be kept in the cache.
        assert_eq!(managed_state.get_cache_len(), 3);

        drop(managed_state);
        let mut managed_state = create_managed_top_n_bottom_n_state(
            &store,
            row_count,
            data_type_kinds.clone(),
            order_types.clone(),
        );
        assert_eq!(managed_state.top_element(), None);
        managed_state.fill_in_cache().await.unwrap();
        // now ("abd", 3) -> ("abc", 3) -> ("ab", 4)
        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[3], &rows[3]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        // Right after recovery.
        assert!(!managed_state.is_dirty());
        assert_eq!(managed_state.get_cache_len(), 3);

        assert_eq!(
            managed_state.pop_top_element().await.unwrap(),
            Some((ordered_rows[3].clone(), rows[3].clone()))
        );
        // now ("abd", 3) -> ("abc", 3)
        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[1], &rows[1]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        assert!(managed_state.is_dirty());
        assert_eq!(managed_state.total_count, 2);
        assert_eq!(managed_state.get_cache_len(), 2);
        assert_eq!(
            managed_state.pop_top_element().await.unwrap(),
            Some((ordered_rows[1].clone(), rows[1].clone()))
        );
        // now ("abd", 3)
        assert!(managed_state.is_dirty());
        assert_eq!(managed_state.total_count, 1);
        assert_eq!(managed_state.get_cache_len(), 1);

        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        managed_state.flush().await.unwrap();
        assert!(!managed_state.is_dirty());

        managed_state
            .insert(ordered_rows[0].clone(), rows[0].clone())
            .await;
        // now ("abd", 3) -> ("abc", 2)
        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[0], &rows[0]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );

        // Exclude the last `insert` as the state crashes before recovery.
        let row_count = managed_state.total_count - 1;
        drop(managed_state);
        let mut managed_state = create_managed_top_n_bottom_n_state(
            &store,
            row_count,
            data_type_kinds.clone(),
            order_types.clone(),
        );
        managed_state.fill_in_cache().await.unwrap();
        assert_eq!(
            managed_state.top_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
        assert_eq!(
            managed_state.bottom_element(),
            Some((&ordered_rows[2], &rows[2]))
        );
    }
}