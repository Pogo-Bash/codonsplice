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

    // Resolve the raw record set: either a deferred producer (variant calling,
    // run now with the LIMIT short-circuit) or eagerly-computed records. The
    // limit hint is only passed when there is neither a per-record predicate nor
    // an ORDER BY: a predicate filters afterwards (an early cap could
    // under-produce), and an ORDER BY must see the full set before truncating —
    // capping the producer first would sort only the first N produced, not the
    // global top N. See #3.
    let limit_hint = if guard.predicate.is_none() && guard.order.is_none() {
        guard.limit.filter(|n| *n >= 0).map(|n| n as usize)
    } else {
        None
    };
    let raw = if let Some(prod) = guard.producer.take() {
        prod(limit_hint)?
    } else {
        guard.records.take().unwrap_or_default()
    };
    let vars = guard.vars.clone();

    // Share rows cheaply with the per-record evaluators.
    let mut rows: Vec<Arc<Record>> = raw.into_iter().map(Arc::new).collect();

    // 1. Predicate.
    if let Some(pred) = &guard.predicate {
        let mut pvm = Vm::eval_only(pred.clone()).with_vars(vars.clone());
        rows.retain(|r| {
            pvm.eval_record(r.clone())
                .map(|v| v.is_truthy())
                .unwrap_or(false)
        });
    }

    // 2. Order by (single key, Phase 4) — evaluated on the original record.
    if let Some((key_prog, desc)) = &guard.order {
        let mut ovm = Vm::eval_only(key_prog.clone()).with_vars(vars.clone());
        let mut keyed: Vec<(RuntimeValue, Arc<Record>)> = rows
            .into_iter()
            .map(|r| {
                let k = ovm.eval_record(r.clone()).unwrap_or(RuntimeValue::Null);
                (k, r)
            })
            .collect();
        // Direction-aware comparator (not sort-then-reverse): `reverse()` would
        // break the stable order of equal-key ties and flip NULLs to the front.
        // NULLs sort last regardless of direction; only the non-null comparison
        // is inverted for DESC. See #3.
        keyed.sort_by(|a, b| match (&a.0, &b.0) {
            (RuntimeValue::Null, RuntimeValue::Null) => Ordering::Equal,
            (RuntimeValue::Null, _) => Ordering::Greater,
            (_, RuntimeValue::Null) => Ordering::Less,
            _ => {
                let ord = cmp_values(&a.0, &b.0);
                if *desc {
                    ord.reverse()
                } else {
                    ord
                }
            }
        });
        rows = keyed.into_iter().map(|(_, r)| r).collect();
    }

    // 3. Limit.
    if let Some(n) = guard.limit {
        if n >= 0 {
            rows.truncate(n as usize);
        }
    }

    // 4. Projection: an explicit, non-wildcard SELECT projects each record to a
    //    Row of (column_name, value) by running each column's sub-program.
    if let Some(items) = &guard.projection {
        if !items.iter().any(|i| i.wildcard) {
            let mut col_vms: Vec<(String, Vm)> = items
                .iter()
                .map(|it| {
                    (
                        it.name.clone(),
                        Vm::eval_only(it.prog.clone()).with_vars(vars.clone()),
                    )
                })
                .collect();
            rows = rows
                .into_iter()
                .map(|r| {
                    let cols: Vec<(String, RuntimeValue)> = col_vms
                        .iter_mut()
                        .map(|(name, vm)| {
                            let v = vm.eval_record(r.clone()).unwrap_or(RuntimeValue::Null);
                            (name.clone(), v)
                        })
                        .collect();
                    Arc::new(Record::Row(cols))
                })
                .collect();
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
