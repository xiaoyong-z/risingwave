package com.risingwave.planner.rel.streaming;

import com.google.common.collect.ImmutableList;
import com.risingwave.catalog.ColumnCatalog;
import com.risingwave.catalog.MaterializedViewCatalog;
import com.risingwave.catalog.TableCatalog;
import com.risingwave.proto.streaming.plan.ChainNode;
import com.risingwave.proto.streaming.plan.StreamNode;
import com.risingwave.rpc.Messages;
import java.util.List;
import org.apache.calcite.plan.RelOptCluster;
import org.apache.calcite.plan.RelOptTable;
import org.apache.calcite.plan.RelTraitSet;
import org.apache.calcite.rel.RelWriter;
import org.apache.calcite.rel.core.TableScan;
import org.apache.calcite.rel.hint.RelHint;
import org.apache.calcite.rel.type.RelDataType;
import org.apache.calcite.rel.type.RelDataTypeFactory;
import org.apache.calcite.util.ImmutableIntList;

/** Chain Node */
public class RwStreamChain extends TableScan implements RisingWaveStreamingRel {

  private final TableCatalog.TableId tableId;
  private final ImmutableList<ColumnCatalog.ColumnId> primaryKeyColumnIds;
  private final ImmutableList<ColumnCatalog.ColumnId> columnIds;
  private final ImmutableIntList primaryKeyIndices;

  /**
   * ChainNode is used to scan materialized view snapshot and its further stream chunks.
   *
   * @param tableId table id of the origin materialized view table.
   * @param primaryKeyColumnIds column ids of the origin materialized view table.
   * @param primaryKeyIndices derived pk indices of chain output.
   * @param columnIds column ids of the origin materialized view.
   */
  public RwStreamChain(
      RelOptCluster cluster,
      RelTraitSet traitSet,
      List<RelHint> hints,
      RelOptTable table,
      TableCatalog.TableId tableId,
      ImmutableList<ColumnCatalog.ColumnId> primaryKeyColumnIds,
      ImmutableIntList primaryKeyIndices,
      ImmutableList<ColumnCatalog.ColumnId> columnIds) {
    super(cluster, traitSet, hints, table);
    this.tableId = tableId;
    this.primaryKeyColumnIds = primaryKeyColumnIds;
    this.primaryKeyIndices = primaryKeyIndices;
    this.columnIds = columnIds;
  }

  public TableCatalog.TableId getTableId() {
    return tableId;
  }

  public ImmutableList<ColumnCatalog.ColumnId> getPrimaryKeyColumnIds() {
    return primaryKeyColumnIds;
  }

  public ImmutableIntList getPrimaryKeyIndices() {
    return primaryKeyIndices;
  }

  public ImmutableList<ColumnCatalog.ColumnId> getColumnIds() {
    return columnIds;
  }

  /** Derive row type from table catalog */
  @Override
  public RelDataType deriveRowType() {
    RelDataTypeFactory.Builder typeBuilder = getCluster().getTypeFactory().builder();
    MaterializedViewCatalog materializedViewCatalog =
        getTable().unwrapOrThrow(MaterializedViewCatalog.class);
    columnIds.stream()
        .map(materializedViewCatalog::getColumnChecked)
        .forEachOrdered(
            col -> typeBuilder.add(col.getEntityName().getValue(), col.getDesc().getDataType()));
    return typeBuilder.build();
  }

  /** Explain */
  @Override
  public RelWriter explainTerms(RelWriter pw) {
    var writer =
        super.explainTerms(pw)
            .item("tableId", tableId)
            .item("primaryKeyIndices", primaryKeyIndices)
            .item("columnIds", columnIds);
    return writer;
  }

  @Override
  public StreamNode serialize() {
    ChainNode.Builder builder = ChainNode.newBuilder();
    builder.setTableRefId(Messages.getTableRefId(tableId)).addAllPkIndices(primaryKeyIndices);
    columnIds.forEach(c -> builder.addColumnIds(c.getValue()));
    ChainNode chainNode = builder.build();
    return StreamNode.newBuilder().setChainNode(chainNode).build();
  }

  @Override
  public <T> RwStreamingRelVisitor.Result<T> accept(RwStreamingRelVisitor<T> visitor) {
    return visitor.visit(this);
  }
}
