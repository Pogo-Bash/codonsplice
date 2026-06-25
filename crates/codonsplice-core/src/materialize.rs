//! Streaming record materialization.
//!
//! After a `CALL_*` opcode fills a cursor's record buffer, materialization pulls
//! the rows out applying, in order:
//!
//! 1. the compiled `WHERE` predicate (per record, via a lightweight eval-only
//!    [`Vm`]),
//! 2. the `ORDER BY` sort key,
//! 3. the `LIMIT` truncation.
//!
//! Region filtering happens earlier (at scan time, via BAI seeking) so records
//! outside the region are never loaded; the predicate here handles the
//! non-region clauses (`af > 0.1`, `mapq >= 30`, …).
//!
//! Records are wrapped in `Arc` while the predicate/order VMs hold them, so
//! per-record evaluation shares the row without deep-cloning it.

use std::cmp::Ordering;
use std::sync::{Arc, Mutex};

use cnvlens_core::error::CoreError;

use crate::runtime::{Cursor, Record, RuntimeValue};
use crate::vm::Vm;

/// Materialize a cursor into a concrete record vector, applying predicate,
/// ordering, and limit. `source` is the original query text (reserved for
/// richer error messages).
pub fn materialize(cursor: Arc<Mutex<Cursor>>, _source: &str) -> Result<Vec<Record>, CoreError> {
    let mut guard = cursor.lock().unwrap();
    let raw = guard.records.take().unwrap_or_default();

    // Share rows cheaply with the per-record evaluators.
    let mut rows: Vec<Arc<Record>> = raw.into_iter().map(Arc::new).collect();

    // 1. Predicate.
    if let Some(pred) = &guard.predicate {
        let mut pvm = Vm::eval_only(pred.clone());
        rows.retain(|r| {
            pvm.eval_record(r.clone())
                .map(|v| v.is_truthy())
                .unwrap_or(false)
        });
    }

    // 2. Order by (single key, Phase 4).
    if let Some((key_prog, desc)) = &guard.order {
        let mut ovm = Vm::eval_only(key_prog.clone());
        let mut keyed: Vec<(RuntimeValue, Arc<Record>)> = rows
            .into_iter()
            .map(|r| {
                let k = ovm.eval_record(r.clone()).unwrap_or(RuntimeValue::Null);
                (k, r)
            })
            .collect();
        keyed.sort_by(|a, b| cmp_values(&a.0, &b.0));
        if *desc {
            keyed.reverse();
        }
        rows = keyed.into_iter().map(|(_, r)| r).collect();
    }

    // 3. Limit.
    if let Some(n) = guard.limit {
        if n >= 0 {
            rows.truncate(n as usize);
        }
    }

    // Unwrap the Arcs back to owned records (refcount is 1 once the VMs drop).
    Ok(rows
        .into_iter()
        .map(|r| Arc::try_unwrap(r).unwrap_or_else(|a| (*a).clone()))
        .collect())
}

/// Streaming variant of [`materialize`]: yields each record as a `Result`. The
/// predicate/order/limit are applied eagerly (ORDER BY must buffer to sort), then
/// the buffered rows are streamed; on error a single `Err` item is yielded.
pub fn materialize_streaming(
    cursor: Arc<Mutex<Cursor>>,
) -> impl Iterator<Item = Result<Record, CoreError>> {
    let items: Vec<Result<Record, CoreError>> = match materialize(cursor, "") {
        Ok(rows) => rows.into_iter().map(Ok).collect(),
        Err(e) => vec![Err(e)],
    };
    items.into_iter()
}

/// Total order over runtime values for `ORDER BY`. Numbers compare numerically,
/// strings lexically, `Null` sorts last; mixed/other types are treated as equal.
fn cmp_values(a: &RuntimeValue, b: &RuntimeValue) -> Ordering {
    match (a, b) {
        (RuntimeValue::Null, RuntimeValue::Null) => Ordering::Equal,
        (RuntimeValue::Null, _) => Ordering::Greater,
        (_, RuntimeValue::Null) => Ordering::Less,
        _ => {
            if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
                return x.partial_cmp(&y).unwrap_or(Ordering::Equal);
            }
            match (a, b) {
                (RuntimeValue::Str(x), RuntimeValue::Str(y)) => x.cmp(y),
                _ => Ordering::Equal,
            }
        }
    }
}
