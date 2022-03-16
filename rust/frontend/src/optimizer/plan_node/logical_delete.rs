use std::fmt;

use fixedbitset::FixedBitSet;
use risingwave_common::catalog::{Field, Schema};
use risingwave_common::error::Result;
use risingwave_common::types::DataType;

use super::{BatchDelete, ColPrunable, LogicalBase, PlanRef, PlanTreeNodeUnary, ToBatch, ToStream};
use crate::binder::BaseTableRef;

/// [`LogicalDelete`] iterates on input relation and delete the data from specified table.
///
/// It corresponds to the `DELETE` statements in SQL.
#[derive(Debug, Clone)]
pub struct LogicalDelete {
    pub base: LogicalBase,
    table: BaseTableRef,
    input: PlanRef,
}

impl LogicalDelete {
    /// Create a [`LogicalDelete`] node. Used internally by optimizer.
    pub fn new(input: PlanRef, table: BaseTableRef) -> Self {
        let ctx = input.ctx();
        // TODO: support `RETURNING`.
        let schema = Schema::new(vec![Field::unnamed(DataType::Int64)]);
        let id = ctx.borrow_mut().get_id();
        let base = LogicalBase { id, schema, ctx };

        Self { base, table, input }
    }

    /// Create a [`LogicalDelete`] node. Used by planner.
    pub fn create(input: PlanRef, table: BaseTableRef) -> Result<Self> {
        Ok(Self::new(input, table))
    }

    pub(super) fn fmt_with_name(&self, f: &mut fmt::Formatter, name: &str) -> fmt::Result {
        f.debug_struct(name)
            .field("table_name", &self.table.name)
            .finish()
    }
}

impl PlanTreeNodeUnary for LogicalDelete {
    fn input(&self) -> PlanRef {
        self.input.clone()
    }

    fn clone_with_input(&self, input: PlanRef) -> Self {
        Self::new(input, self.table.clone())
    }
}

impl_plan_tree_node_for_unary! { LogicalDelete }

impl fmt::Display for LogicalDelete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_name(f, "LogicalDelete")
    }
}

impl ColPrunable for LogicalDelete {
    fn prune_col(&self, _required_cols: &FixedBitSet) -> PlanRef {
        let mut all_cols = FixedBitSet::with_capacity(self.input.schema().len());
        all_cols.insert_range(..);
        self.clone_with_input(self.input.prune_col(&all_cols))
            .into()
    }
}

impl ToBatch for LogicalDelete {
    fn to_batch(&self) -> PlanRef {
        let new_input = self.input().to_batch();
        let new_logical = self.clone_with_input(new_input);
        BatchDelete::new(new_logical).into()
    }
}

impl ToStream for LogicalDelete {
    fn to_stream(&self) -> PlanRef {
        unreachable!("delete should always be converted to batch plan");
    }
}