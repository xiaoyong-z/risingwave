use async_trait::async_trait;
use risingwave_common::catalog::Schema;
use risingwave_common::error::{ErrorCode, Result};

use super::{Executor, Message, PkIndicesRef};

#[derive(Debug)]
enum ChainState {
    ReadingSnapshot,
    ReadingMView,
}

/// [`Chain`] is an operator that enables synchronization between the existing stream and newly
/// appended executors. Currently, [`Chain`] is mainly used to implement MV on MV feature. It pipes
/// new data of existing MVs to newly created MV only all of the old data in the existing MVs are
/// dispatched.
#[derive(Debug)]
pub struct Chain {
    snapshot: Box<dyn Executor>,
    mview: Box<dyn Executor>,
    state: ChainState,
}

impl Chain {
    pub fn new(snapshot: Box<dyn Executor>, mview: Box<dyn Executor>) -> Self {
        Self {
            snapshot,
            mview,
            state: ChainState::ReadingSnapshot,
        }
    }

    async fn read_mview(&mut self) -> Result<Message> {
        self.mview.next().await
    }
    async fn read_snapshot(&mut self) -> Result<Message> {
        self.snapshot.next().await
    }
    async fn switch_and_read_mview(&mut self) -> Result<Message> {
        self.state = ChainState::ReadingMView;
        return self.read_mview().await;
    }
    async fn next_inner(&mut self) -> Result<Message> {
        match &self.state {
            ChainState::ReadingSnapshot => match self.snapshot.next().await {
                Err(e) => {
                    // TODO: Refactor this once we find a better way to know the upstream is done.
                    if let ErrorCode::EOF = e.inner() {
                        return self.switch_and_read_mview().await;
                    }
                    Err(e)
                }
                Ok(msg) => Ok(msg),
            },
            ChainState::ReadingMView => self.read_mview().await,
        }
    }
}

#[async_trait]
impl Executor for Chain {
    async fn next(&mut self) -> Result<Message> {
        self.next_inner().await
    }

    fn schema(&self) -> &Schema {
        self.mview.schema()
    }

    fn pk_indices(&self) -> PkIndicesRef {
        self.mview.pk_indices()
    }

    fn identity(&self) -> &'static str {
        "Chain"
    }
}

#[cfg(test)]
mod test {

    use async_trait::async_trait;
    use risingwave_common::array::{Array, I32Array, Op, RwError, StreamChunk};
    use risingwave_common::catalog::Schema;
    use risingwave_common::error::ErrorCode;
    use risingwave_pb::data::data_type::TypeName;
    use risingwave_pb::data::DataType;
    use risingwave_pb::plan::column_desc::ColumnEncodingType;
    use risingwave_pb::plan::ColumnDesc;

    use super::Chain;
    use crate::executor::test_utils::MockSource;
    use crate::executor::{Executor, Message, PkIndices, PkIndicesRef};
    use crate::risingwave_common::error::Result;

    #[derive(Debug)]
    struct MockSnapshot(MockSource);

    impl MockSnapshot {
        pub fn with_chunks(
            schema: Schema,
            pk_indices: PkIndices,
            chunks: Vec<StreamChunk>,
        ) -> Self {
            Self(MockSource::with_chunks(schema, pk_indices, chunks))
        }

        async fn next_inner(&mut self) -> Result<Message> {
            match self.0.next().await {
                Ok(m) => {
                    if let Message::Barrier(_) = m {
                        // warning: translate all of the barrier types to the EOF here. May be an
                        // error in some circumstances.
                        Err(RwError::from(ErrorCode::EOF))
                    } else {
                        Ok(m)
                    }
                }
                Err(e) => Err(e),
            }
        }
    }

    #[async_trait]
    impl Executor for MockSnapshot {
        async fn next(&mut self) -> Result<Message> {
            self.next_inner().await
        }

        fn schema(&self) -> &Schema {
            self.0.schema()
        }

        fn pk_indices(&self) -> PkIndicesRef {
            self.0.pk_indices()
        }

        fn identity(&self) -> &'static str {
            "MockSnapshot"
        }
    }

    #[tokio::test]
    async fn test_basic() {
        let columns = vec![ColumnDesc {
            column_type: Some(DataType {
                type_name: TypeName::Int32 as i32,
                ..Default::default()
            }),
            encoding: ColumnEncodingType::Raw as i32,
            name: "v1".to_string(),
            is_primary: false,
            column_id: 0,
        }];
        let schema = Schema::try_from(&columns).unwrap();
        let first = Box::new(MockSnapshot::with_chunks(
            schema.clone(),
            PkIndices::new(),
            vec![
                StreamChunk::new(
                    vec![Op::Insert],
                    vec![column_nonnull! { I32Array, [1] }],
                    None,
                ),
                StreamChunk::new(
                    vec![Op::Insert],
                    vec![column_nonnull! { I32Array, [2] }],
                    None,
                ),
            ],
        ));
        let second = Box::new(MockSource::with_chunks(
            schema.clone(),
            PkIndices::new(),
            vec![
                StreamChunk::new(
                    vec![Op::Insert],
                    vec![column_nonnull! { I32Array, [3] }],
                    None,
                ),
                StreamChunk::new(
                    vec![Op::Insert],
                    vec![column_nonnull! { I32Array, [4] }],
                    None,
                ),
            ],
        ));

        let mut chain = Chain::new(first, second);
        let mut count = 0;
        loop {
            let k = &chain.next().await.unwrap();
            count += 1;
            if let Message::Chunk(ck) = k {
                let target = ck.column(0).array_ref().as_int32().value_at(0).unwrap();
                assert_eq!(target, count);
            } else {
                assert!(matches!(k, Message::Barrier(_)));
                return;
            }
        }
    }
}