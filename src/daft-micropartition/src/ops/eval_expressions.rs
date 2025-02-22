use std::{collections::HashSet, sync::Arc};

use common_error::{DaftError, DaftResult};
use daft_core::schema::Schema;
use daft_dsl::Expr;
use snafu::ResultExt;

use crate::{
    micropartition::{MicroPartition, TableState},
    DaftCoreComputeSnafu,
};

use daft_stats::{ColumnRangeStatistics, TableStatistics};

use daft_stats::TableMetadata;

fn infer_schema(exprs: &[Expr], schema: &Schema) -> DaftResult<Schema> {
    let fields = exprs
        .iter()
        .map(|e| e.to_field(schema).context(DaftCoreComputeSnafu))
        .collect::<crate::Result<Vec<_>>>()?;

    let mut seen: HashSet<String> = HashSet::new();
    for field in fields.iter() {
        let name = &field.name;
        if seen.contains(name) {
            return Err(DaftError::ValueError(format!(
                "Duplicate name found when evaluating expressions: {name}"
            )));
        }
        seen.insert(name.clone());
    }
    Schema::new(fields)
}

impl MicroPartition {
    pub fn eval_expression_list(&self, exprs: &[Expr]) -> DaftResult<Self> {
        let expected_schema = infer_schema(exprs, &self.schema)?;
        let tables = self.tables_or_read(None)?;
        let evaluated_tables = tables
            .iter()
            .map(|t| t.eval_expression_list(exprs))
            .collect::<DaftResult<Vec<_>>>()?;

        let eval_stats = self
            .statistics
            .as_ref()
            .map(|s| s.eval_expression_list(exprs, &expected_schema))
            .transpose()?;

        Ok(MicroPartition::new(
            expected_schema.into(),
            TableState::Loaded(Arc::new(evaluated_tables)),
            TableMetadata { length: self.len() },
            eval_stats,
        ))
    }

    pub fn explode(&self, exprs: &[Expr]) -> DaftResult<Self> {
        let tables = self.tables_or_read(None)?;
        let evaluated_tables = tables
            .iter()
            .map(|t| t.explode(exprs))
            .collect::<DaftResult<Vec<_>>>()?;
        let expected_new_columns = infer_schema(exprs, &self.schema)?;
        let eval_stats = if let Some(stats) = &self.statistics {
            let mut new_stats = stats.columns.clone();
            for (name, _) in expected_new_columns.fields.iter() {
                if let Some(v) = new_stats.get_mut(name) {
                    *v = ColumnRangeStatistics::Missing;
                } else {
                    new_stats.insert(name.to_string(), ColumnRangeStatistics::Missing);
                }
            }
            Some(TableStatistics { columns: new_stats })
        } else {
            None
        };

        let mut expected_schema =
            Schema::new(self.schema.fields.values().cloned().collect::<Vec<_>>())?;
        for (name, field) in expected_new_columns.fields.into_iter() {
            if let Some(v) = expected_schema.fields.get_mut(&name) {
                *v = field;
            } else {
                expected_schema.fields.insert(name.to_string(), field);
            }
        }

        let new_len = evaluated_tables.iter().map(|t| t.len()).sum();

        Ok(MicroPartition::new(
            Arc::new(expected_schema),
            TableState::Loaded(Arc::new(evaluated_tables)),
            TableMetadata { length: new_len },
            eval_stats,
        ))
    }
}
