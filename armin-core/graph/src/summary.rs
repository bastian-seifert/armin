use std::time::{SystemTime, UNIX_EPOCH};

use crate::debt::compute_debt;
use crate::decisions::extract_decisions;
use crate::risks::compute_risks;
use crate::store::GraphStoreInner;
use crate::types::{DebtDelta, DebtReport, ExecutiveSummary};
use crate::utils::session_id_at_index;

/// Aggregate an executive summary from the current graph state.
pub fn compute_summary(
    inner: &GraphStoreInner,
    current_session_idx: usize,
    prior_debt: Option<&DebtReport>,
) -> ExecutiveSummary {
    let decisions = extract_decisions(inner, current_session_idx);
    let risks = compute_risks(inner, current_session_idx);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let current_debt = compute_debt(inner, current_session_idx, now);

    let debt_delta = match prior_debt {
        Some(prior) => compute_debt_delta(&current_debt, prior),
        None => DebtDelta {
            new: current_debt.items.len() as u32,
            resolved: 0,
            persisted: 0,
        },
    };

    let session_id = session_id_at_index(&inner.session_order, current_session_idx)
        .unwrap_or_else(|| "Unknown".to_string());

    let prior_session_id = if current_session_idx > 0 {
        session_id_at_index(&inner.session_order, current_session_idx - 1)
    } else {
        None
    };

    ExecutiveSummary {
        decisions,
        risks,
        debt_delta,
        session_id,
        prior_session_id,
        generated_at: now,
    }
}

/// Compute the delta between current and prior debt reports.
///
/// Two debt items are considered "the same" if they share the same debt_type
/// and the same sorted set of node_ids.
fn compute_debt_delta(current: &DebtReport, prior: &DebtReport) -> DebtDelta {
    let current_keys: Vec<(String, Vec<String>)> = current
        .items
        .iter()
        .map(|i| {
            let mut ids = i.node_ids.clone();
            ids.sort();
            (i.debt_type.clone(), ids)
        })
        .collect();

    let prior_keys: Vec<(String, Vec<String>)> = prior
        .items
        .iter()
        .map(|i| {
            let mut ids = i.node_ids.clone();
            ids.sort();
            (i.debt_type.clone(), ids)
        })
        .collect();

    let current_set: std::collections::HashSet<(String, Vec<String>)> =
        current_keys.into_iter().collect();
    let prior_set: std::collections::HashSet<(String, Vec<String>)> =
        prior_keys.into_iter().collect();

    let new = current_set.difference(&prior_set).count() as u32;
    let resolved = prior_set.difference(&current_set).count() as u32;
    let persisted = current_set.intersection(&prior_set).count() as u32;

    DebtDelta {
        new,
        resolved,
        persisted,
    }
}
