package com.risingwave.planner.rel.streaming;

import com.google.common.collect.ImmutableList;
import com.google.common.collect.ImmutableMap;
import com.risingwave.catalog.ColumnCatalog;
import com.risingwave.catalog.TableCatalog;
import com.risingwave.planner.rel.common.dist.RwDistributionTrait;
import com.risingwave.planner.rel.streaming.join.RwStreamHashJoin;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import org.apache.calcite.rel.RelCollations;
import org.apache.calcite.rel.RelFieldCollation;
import org.apache.calcite.rex.RexInputRef;
import org.apache.calcite.rex.RexNode;
import org.apache.calcite.rex.RexShuttle;
import org.apache.calcite.util.Permutation;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

/** This visitor is used to construct the new plan and also the primary key for each operator. */
public class PrimaryKeyDerivationVisitor
    implements RwStreamingRelVisitor<PrimaryKeyDerivationVisitor.PrimaryKeyIndicesAndPositionMap> {
  private static final Logger LOGGER = LoggerFactory.getLogger(PrimaryKeyDerivationVisitor.class);

  /**
   * This class contains two things:
   *
   * <p>1. A list to indicate the indices of primary key among all the columns represented by the
   * row type. We remark that we denote primary key by a list instead of a set as the order of the
   * keys matters in certain cases. For example, select v1, v2, count(*) from t group by v1, v2. In
   * this case, the order of v1, v2 does not matter. However, select v1, v2, count(*) from t group
   * by v1, v2 order by v2. In this case, the order has to be [v2, v1] but not [v1, v2].
   *
   * <p>2. A map for the new child to tell its parent that some original columns have been changed
   * into some other position. See comments for hash join.
   */
  public class PrimaryKeyIndicesAndPositionMap {
    private final ImmutableList<Integer> primaryKeyIndices;
    // Since the new child can reorder its columns in its row type and thus mess up the index of
    // input ref in parent operator,
    // we need to a positionMap to renumber the index in input ref.
    private final ImmutableMap<Integer, Integer> positionMap;

    public PrimaryKeyIndicesAndPositionMap(
        ImmutableList<Integer> primaryKeyIndices, ImmutableMap<Integer, Integer> positionMap) {
      this.primaryKeyIndices = primaryKeyIndices;
      this.positionMap = positionMap;
    }

    public ImmutableList<Integer> getPrimaryKeyIndices() {
      return primaryKeyIndices;
    }

    public ImmutableMap<Integer, Integer> getPositionMap() {
      return positionMap;
    }
  }

  /**
   * FIXME: If we select some columns instead of * from the join result, there will be a project on
   * top of the join operator. This project removes some columns returned by Join. It cannot know
   * that it still needs to remove that column if that column is returned as primary key from the
   * new child, in this case the join operator, as the new child can indeed add some new columns as
   * part of the primary key. Hence, this function is solely designed for select * from xxx join
   * xxx;
   *
   * <p>We need another one to specifically address project -> join.
   *
   * <p>Here are some explicit or implicit assumptions/conditions enforced by calcite:
   *
   * <p>1. The hash join's row type is derived by concatenating the row type of left input and row
   * type of right input. See Join's deriveRowType.
   *
   * <p>2. The leftKeys and rightKeys of JoinInfo are one-to-one correspondent.
   *
   * <p>3. The leftKeys and rightKeys refer to the position of the output row type of join.
   * RightKeys do NOT refer to the positions in the output row type of right child.
   *
   * <p>The invariant we try to maintain in this primary key derivation process is that the newly
   * added columns of children are always appended at last. So it will not mess up the index in
   * original InputRef RexNodes of some upstream operator.
   *
   * <p>However, this invariant can only be maintained for non-Join operator. If the new left child
   * of a join operator has newly added columns in its row type, then these columns will be in the
   * middle of the output row type of join operator. And the row type of the new right child will be
   * appended behind that. Therefore, it brings additional complexity. This is why we introduce
   * positionMap.
   *
   * @param hashJoin Join's primary key derivation rule is: outer_input.key() || inner_input.key()
   * @return New join and its output primary key.
   */
  @Override
  public RwStreamingRelVisitor.Result<PrimaryKeyIndicesAndPositionMap> visit(
      RwStreamHashJoin hashJoin) {
    LOGGER.debug("visit RwStreamHashJoin");
    RisingWaveStreamingRel leftInput = (RisingWaveStreamingRel) hashJoin.getInput(0);
    var originalLeftFieldCount = leftInput.getRowType().getFieldCount();
    var leftRes = leftInput.accept(this);
    var leftPositionMap = leftRes.info.getPositionMap();
    var newLeftFieldCount = leftRes.node.getRowType().getFieldCount();
    RisingWaveStreamingRel rightInput = (RisingWaveStreamingRel) hashJoin.getInput(1);
    var originalRightFieldCount = rightInput.getRowType().getFieldCount();
    var rightRes = rightInput.accept(this);
    var rightPositionMap = leftRes.info.getPositionMap();
    var originalTotalFieldCount = originalLeftFieldCount + originalRightFieldCount;

    var joinInfo = hashJoin.analyzeCondition();
    var joinCondition = hashJoin.getCondition();

    var joinType = hashJoin.getJoinType();
    LOGGER.debug("originalLeftInput row type:" + leftInput.getRowType());
    LOGGER.debug("newLeftInput row type:" + leftRes.node.getRowType());
    LOGGER.debug("originalRightInput row type:" + rightInput.getRowType());
    LOGGER.debug("newLeftInput row type:" + rightRes.node.getRowType());

    // Since the left child may add new columns, this would invalidate rightJoinKeyIndices.
    // Therefore, we need to renumber it. Besides, the new children may itself has reordered
    // its columns.
    // We need to reindex for both of these two cases.
    RexShuttle inputRefReplaceShuttle =
        new RexShuttle() {
          @Override
          public RexNode visitInputRef(RexInputRef inputRef) {
            var index = inputRef.getIndex();
            // We remark that this index will not refer to any new column added by the new left
            // child
            // and the new right child.
            if (index >= originalLeftFieldCount) {
              // This index refers to some column from right child.
              // We remark again that this index is for the row type of the join, left row type ||
              // right row type.
              // So we need to first translate it as a true `right index`.
              var rightIndex = index - originalLeftFieldCount;
              var newRightIndex = rightPositionMap.getOrDefault(rightIndex, rightIndex);
              return new RexInputRef(newRightIndex + newLeftFieldCount, inputRef.getType());
            } else {
              // This index refers to some column from left child.
              var newIndex = leftPositionMap.getOrDefault(index, index);
              return new RexInputRef(newIndex, inputRef.getType());
            }
          }
        };
    RexNode newJoinCondition = joinCondition.accept(inputRefReplaceShuttle);
    var newJoinPositionMap = new HashMap<Integer, Integer>();
    for (int i = originalLeftFieldCount; i < originalTotalFieldCount; i++) {
      newJoinPositionMap.putIfAbsent(i, i + newLeftFieldCount - originalLeftFieldCount);
    }
    LOGGER.debug("new join position map:" + newJoinPositionMap);
    LOGGER.debug("join condition:" + joinCondition);
    LOGGER.debug("new join condition:" + newJoinCondition);
    var newHashJoin =
        new RwStreamHashJoin(
            hashJoin.getCluster(),
            hashJoin.getTraitSet(),
            hashJoin.getHints(),
            leftRes.node,
            rightRes.node,
            newJoinCondition,
            joinType);

    List<Integer> primaryKeyIndices = new ArrayList<Integer>();
    LOGGER.debug("visit join type:" + joinType);
    switch (joinType) {
      case INNER:
      case LEFT:
      case FULL:
        // We remark that we can process INNER, LEFT and FULL in the same way as in both cases
        // we put the primary key indices of left child first, and right child second.
        var rightJoinKeyIndices = new HashSet<Integer>();
        var leftToRightJoinKeyIndices = new HashMap<Integer, Integer>();
        for (var p : joinInfo.pairs()) {
          // We remark that target is left key index, source is right key index
          leftToRightJoinKeyIndices.putIfAbsent(p.source, p.target);
        }
        LOGGER.debug("leftToRightJoinKeyIndices:" + leftToRightJoinKeyIndices);
        LOGGER.debug("rightJoinKeyIndices:" + rightJoinKeyIndices);

        // First add all the output key from new left child
        for (var newLeftPrimaryKeyIndex : leftRes.info.getPrimaryKeyIndices()) {
          primaryKeyIndices.add(newLeftPrimaryKeyIndex);
          if (leftToRightJoinKeyIndices.containsKey(newLeftPrimaryKeyIndex)) {
            rightJoinKeyIndices.add(leftToRightJoinKeyIndices.get(newLeftPrimaryKeyIndex));
          }
        }
        // It is possible that the output key of left child may be a join key
        // We do NOT want to add the same join key twice when we process new right child.
        // But we DO need to add other non-join-key primary key from the new right child.
        for (var newRightPrimaryKeyIndex : rightRes.info.getPrimaryKeyIndices()) {
          if (!rightJoinKeyIndices.contains(newRightPrimaryKeyIndex + originalLeftFieldCount)) {
            primaryKeyIndices.add(newRightPrimaryKeyIndex + newLeftFieldCount);
          }
        }
        break;
      case RIGHT:
        var leftJoinKeyIndices = new HashSet<Integer>();
        var rightToLeftJoinKeyIndices = new HashMap<Integer, Integer>();
        for (var p : joinInfo.pairs()) {
          // We remark that target is left key index, source is right key index.
          rightToLeftJoinKeyIndices.putIfAbsent(p.target, p.source);
        }
        LOGGER.debug("rightToLeftJoinKeyIndices:" + rightToLeftJoinKeyIndices);
        LOGGER.debug("leftJoinKeyIndices:" + leftJoinKeyIndices);

        // We put the primary key of right child first, and left child second.
        for (var newRightPrimaryKeyIndex : rightRes.info.getPrimaryKeyIndices()) {
          primaryKeyIndices.add(newRightPrimaryKeyIndex + newLeftFieldCount);
          if (rightToLeftJoinKeyIndices.containsKey(
              newRightPrimaryKeyIndex + originalLeftFieldCount)) {
            leftJoinKeyIndices.add(
                rightToLeftJoinKeyIndices.get(newRightPrimaryKeyIndex + originalLeftFieldCount));
          }
        }

        for (var newLeftPrimaryKeyIndex : leftRes.info.getPrimaryKeyIndices()) {
          if (!leftJoinKeyIndices.contains(newLeftPrimaryKeyIndex)) {
            primaryKeyIndices.add(newLeftPrimaryKeyIndex);
          }
        }
        break;
      default:
        throw new IllegalArgumentException("Only support inner hash join now");
    }
    LOGGER.debug("primary key indices:" + primaryKeyIndices);
    var info =
        new PrimaryKeyIndicesAndPositionMap(
            ImmutableList.copyOf(primaryKeyIndices), ImmutableMap.copyOf(newJoinPositionMap));
    LOGGER.debug("leave RwStreamHashJoin");
    return new Result<>(newHashJoin, info);
  }

  /**
   * Sort concatenates its sort keys and input primary key as its output primary key. If they
   * overlap, the overlap part will be kept only once.
   *
   * @param sort Original sort
   * @return Sort and its output primary key
   */
  @Override
  public Result<PrimaryKeyIndicesAndPositionMap> visit(RwStreamSort sort) {
    LOGGER.debug("visit RwStreamSort");
    var input = (RisingWaveStreamingRel) sort.getInput(0);
    var p = input.accept(this);
    var positionMap = p.info.getPositionMap();
    var oldPrimaryKeyIndices = p.info.getPrimaryKeyIndices();
    var newPrimaryKeyIndices = new ArrayList<Integer>();

    var newFieldCollations = new ArrayList<RelFieldCollation>();
    for (var collation : sort.collation.getFieldCollations()) {
      var index = collation.getFieldIndex();
      var newIndex = positionMap.getOrDefault(index, index);
      newPrimaryKeyIndices.add(newIndex);
      var newCollation = collation.withFieldIndex(index);
      newFieldCollations.add(newCollation);
    }
    for (var oldPrimaryKeyIndex : oldPrimaryKeyIndices) {
      boolean exist = false;
      for (var existPrimaryKeyIndex : newPrimaryKeyIndices) {
        if (oldPrimaryKeyIndex.equals(existPrimaryKeyIndex)) {
          exist = true;
          break;
        }
      }
      if (!exist) {
        newPrimaryKeyIndices.add(oldPrimaryKeyIndex);
      }
    }
    var newCollation = RelCollations.of(sort.collation.getFieldCollations());
    LOGGER.debug("old collation:" + sort.collation);
    LOGGER.debug("new collation:" + newCollation);
    LOGGER.debug("old primaryKeyIndices:" + oldPrimaryKeyIndices);
    LOGGER.debug("new primaryKeyIndices:" + newPrimaryKeyIndices);
    var newSort =
        (RwStreamSort) sort.copy(sort.getTraitSet(), p.node, newCollation, sort.offset, sort.fetch);
    return new Result<>(
        newSort,
        new PrimaryKeyIndicesAndPositionMap(
            ImmutableList.copyOf(newPrimaryKeyIndices), positionMap));
  }

  /**
   * @param aggregate Aggregate's group by key is its output key.
   * @return Original aggregate
   */
  @Override
  public RwStreamingRelVisitor.Result<PrimaryKeyIndicesAndPositionMap> visit(
      RwStreamAgg aggregate) {
    LOGGER.debug("visit RwStreamAgg");
    // Although we don't need to go beyond Aggregate to find the primary key for materialized
    // view(root),
    // the downstream operator(aggregate, join) may need to know the primary key for themselves.
    // Therefore, we still need to recursively go down.
    var input = (RisingWaveStreamingRel) aggregate.getInput(0);
    var p = input.accept(this);
    var groupSet = aggregate.getGroupSet();
    // If the aggregate is a simple aggregate, it has no primary key or let's say it has a one and
    // only unique key.
    // This is fine because we would have only one row.
    var groupList = ImmutableList.copyOf(groupSet);
    var info = new PrimaryKeyIndicesAndPositionMap(groupList, ImmutableMap.of());
    RwStreamAgg newAggregate =
        (RwStreamAgg) aggregate.copy(aggregate.getTraitSet(), List.of(input));
    LOGGER.debug("leave RwStreamAgg");
    return new Result<>(newAggregate, info);
  }

  /**
   * @param exchange Exchange does not alter primary key and input columns, just pass its input's
   *     primary key and columns up
   * @return Exchange and its output primary key
   */
  @Override
  public RwStreamingRelVisitor.Result<PrimaryKeyIndicesAndPositionMap> visit(
      RwStreamExchange exchange) {
    LOGGER.debug("visit RwStreamExchange");
    var input = (RisingWaveStreamingRel) exchange.getInput(0);
    var p = input.accept(this);

    int[] target = new int[p.node.getRowType().getFieldCount()];
    for (int i = 0; i < target.length; i++) {
      target[i] = i;
    }
    for (var originalToNew : p.info.getPositionMap().entrySet()) {
      target[originalToNew.getKey()] = originalToNew.getValue();
    }
    Permutation permutation = new Permutation(target);
    var newDistribution = (RwDistributionTrait) exchange.getDistribution().apply(permutation);
    var newExchange = RwStreamExchange.create(p.node, newDistribution);
    var info =
        new PrimaryKeyIndicesAndPositionMap(p.info.getPrimaryKeyIndices(), ImmutableMap.of());
    LOGGER.debug("leave RwStreamExchange");
    return new Result<>(newExchange, info);
  }

  /**
   * @param filter Filter does not alter primary key and input columns, just pass its input's
   *     primary key and columns up
   * @return Filter and its output primary key
   */
  @Override
  public RwStreamingRelVisitor.Result<PrimaryKeyIndicesAndPositionMap> visit(
      RwStreamFilter filter) {
    LOGGER.debug("visit RwStreamFilter");
    var input = (RisingWaveStreamingRel) filter.getInput();
    var p = input.accept(this);
    var positionMap = p.info.getPositionMap();

    var condition = filter.getCondition();
    RexShuttle inputRefReplaceShuttle =
        new RexShuttle() {
          @Override
          public RexNode visitInputRef(RexInputRef inputRef) {
            var index = inputRef.getIndex();
            if (positionMap.containsKey(index)) {
              return new RexInputRef(positionMap.get(index), inputRef.getType());
            } else {
              return inputRef;
            }
          }
        };
    var newCondition = condition.accept(inputRefReplaceShuttle);
    var newFilter =
        new RwStreamFilter(
            filter.getCluster(), filter.getTraitSet(), filter.getInput(), newCondition);
    var info =
        new PrimaryKeyIndicesAndPositionMap(p.info.getPrimaryKeyIndices(), ImmutableMap.of());
    LOGGER.debug("leave RwStreamFilter");
    return new Result<>(newFilter, info);
  }

  /**
   * Project needs extra care as it must keep the columns of primary key in its output.
   *
   * @param project Project does not alter primary key but DO alter input columns.
   * @return New project and its output primary key
   */
  @Override
  public RwStreamingRelVisitor.Result<PrimaryKeyIndicesAndPositionMap> visit(
      RwStreamProject project) {
    LOGGER.debug("visit RwStreamProject");
    var rexBuilder = project.getCluster().getRexBuilder();
    var originalInputRowType = project.getInput().getRowType();
    LOGGER.debug("originalInputRowType:" + originalInputRowType);

    RisingWaveStreamingRel input = (RisingWaveStreamingRel) project.getInput();
    var p = input.accept(this);
    var positionMap = p.info.getPositionMap();
    var newInputRowType = p.node.getRowType();
    var newInputPrimaryKeyIndices = p.info.getPrimaryKeyIndices();
    var newOutputPrimaryKeyIndices = new ArrayList<Integer>();
    LOGGER.debug("newInputRowType:" + newInputRowType);

    RwStreamProject newProject;

    RexShuttle inputRefReplaceShuttle =
        new RexShuttle() {
          @Override
          public RexNode visitInputRef(RexInputRef inputRef) {
            var index = inputRef.getIndex();
            if (positionMap.containsKey(index)) {
              return new RexInputRef(positionMap.get(index), inputRef.getType());
            } else {
              return inputRef;
            }
          }
        };
    var oldProjectExpressions = new ArrayList<RexNode>();
    for (var oldProjectExpression : project.getProjects()) {
      oldProjectExpressions.add(oldProjectExpression.accept(inputRefReplaceShuttle));
    }
    var newProjects = new ArrayList<>(oldProjectExpressions);
    var newFields = new ArrayList<>(project.getRowType().getFieldList());

    // We try to find whether the primary key of its child has been put into new project's projects:
    // 1. If yes, nothing happen.
    // 2. If not, we have to add a new InputRef so that that primary key will be one of the output
    // column in new project.
    for (int idx = 0; idx < newInputPrimaryKeyIndices.size(); idx++) {
      var primaryKeyIndex = newInputPrimaryKeyIndices.get(idx);
      boolean exist = false;
      for (int newIdx = 0; newIdx < newProjects.size(); newIdx++) {
        var newProjectRex = newProjects.get(newIdx);
        if (newProjectRex instanceof RexInputRef) {
          var inputRefIndex = ((RexInputRef) newProjectRex).getIndex();
          if (primaryKeyIndex == inputRefIndex) {
            exist = true;
            newOutputPrimaryKeyIndices.add(newIdx);
            break;
          }
        }
      }
      // If we miss an input ref to a primary key in the child, we need to add one.
      if (!exist) {
        var field = newInputRowType.getFieldList().get(primaryKeyIndex);
        newProjects.add(rexBuilder.makeInputRef(field.getType(), primaryKeyIndex));
        newFields.add(field);
        newOutputPrimaryKeyIndices.add(newProjects.size() - 1);
      }
    }
    newProject =
        new RwStreamProject(
            project.getCluster(),
            project.getTraitSet(),
            project.getHints(),
            p.node,
            newProjects,
            rexBuilder.getTypeFactory().createStructType(newFields));
    LOGGER.debug("newInputPrimaryKeyIndices:" + newInputPrimaryKeyIndices);
    LOGGER.debug("newOutputPrimaryKeyIndices:" + newOutputPrimaryKeyIndices);
    LOGGER.debug("newProject's child:" + p.node);
    LOGGER.debug("newProject:" + newProject);
    LOGGER.debug("oldProjectExpressions:" + oldProjectExpressions);
    LOGGER.debug("newProjectExpressions:" + newProject.getProjects());
    LOGGER.debug("newFields:" + newFields);
    var info =
        new PrimaryKeyIndicesAndPositionMap(
            ImmutableList.copyOf(newOutputPrimaryKeyIndices), ImmutableMap.of());
    LOGGER.debug("leave RwStreamProject");
    return new Result<>(newProject, info);
  }

  /**
   * @param tableSource column 0 is the row id column.
   * @return A new Table source with possibly row id column added
   */
  @Override
  public RwStreamingRelVisitor.Result<PrimaryKeyIndicesAndPositionMap> visit(
      RwStreamTableSource tableSource) {
    LOGGER.debug("visit RwStreamTableSource");
    var tableCatalog = tableSource.getTable().unwrapOrThrow(TableCatalog.class);
    // There are two cases:
    // 1. We have already read the row id column, thus we don't need to read additionally
    // 2. We don't have the row id column, then we need to read it and put it as its only primary
    // key
    var rowIdColumn = tableCatalog.getRowIdColumn().getId();
    var columnIds = tableSource.getColumnIds();
    for (int idx = 0; idx < columnIds.size(); idx++) {
      var columnId = columnIds.get(idx);
      if (columnId.equals(rowIdColumn)) {
        LOGGER.debug("Already has row id column, no need to read again");
        var info = new PrimaryKeyIndicesAndPositionMap(ImmutableList.of(idx), ImmutableMap.of());
        LOGGER.debug("leave RwStreamTableSource");
        return new Result<>(tableSource, info);
      }
    }
    // The other one is that we add the row id column to the back, otherwise it will mess up the
    // input ref index in the upstream operator.
    var columns =
        ImmutableList.<ColumnCatalog.ColumnId>builder()
            .addAll(tableSource.getColumnIds())
            .add(rowIdColumn)
            .build();
    var newTableSource =
        new RwStreamTableSource(
            tableSource.getCluster(),
            tableSource.getTraitSet(),
            tableSource.getHints(),
            tableSource.getTable(),
            tableSource.getTableId(),
            columns,
            tableCatalog.isStream());
    var info =
        new PrimaryKeyIndicesAndPositionMap(
            ImmutableList.of(columns.size() - 1), ImmutableMap.of());
    LOGGER.debug("leave RwStreamTableSource");
    return new Result<>(newTableSource, info);
  }
}
