//! What the live path records per span between layouts, in one record per span with one place
//! that decides when each fact expires. Before, each fact was a map of its own with its own
//! clearing rule, spread over the session; a fact cleared in one place and kept in another is
//! the stale-state bug this module exists to prevent.
//!
//! Three lifetimes:
//! - **until the next layout** (`on_new_layout`): what the live path guessed and the pass now
//!   knows — a borrowed context, the latest live rows and placement, the counters seen;
//! - **for one layout version** (tagged; other versions are ignored and dropped at the next
//!   layout): a probe verdict, an over-budget mark, an internal error;
//! - **until the preamble changes** (`on_preamble_change`): a leaked definition, a quarantine
//!   after the unit hung or crashed the engine, over-budget strikes.

use super::*;

/// An over-budget mark that is not tied to a layout version: the unit hung or crashed the
/// engine and stays on the full compile until the preamble changes.
pub(super) const QUARANTINED: u64 = u64::MAX;

#[derive(Debug, Default)]
pub(super) struct LiveUnit {
    // --- until the next layout ---
    /// Compiled on a borrowed context: (unit the context came from, span the rows are placed
    /// against, whether they follow it).
    pub derived: Option<(ParaId, ParaId, bool)>,
    /// Row count of the latest live result (anchors a unit placed after it).
    pub rows: Option<i64>,
    /// Where the latest live result was placed: (page, x of its first row, its last baseline);
    /// a unit typed after it on a borrowed context chains onto it.
    pub place: Option<(i64, i64, i64)>,
    /// Counters the latest live compile advanced (a change renumbers what follows: pass).
    pub counters: Option<BTreeMap<String, i64>>,
    // --- for one layout version ---
    /// Probe verdict (probe mode): (layout version, verdict).
    pub probe: Option<(u64, ProbeVerdict)>,
    /// Over the fast budget: (layout version when measured, or QUARANTINED; ms).
    pub over_budget: Option<(u64, u64)>,
    /// A Lua error of the server's own on this unit: (layout version, message). The unit waits
    /// for the pass until the next layout, then gets another chance.
    pub internal_error: Option<(u64, String)>,
    // --- until the preamble changes ---
    /// The compile changed the meaning of a control sequence (a definition leaked).
    pub leak: Option<String>,
    /// Consecutive over-budget compiles (the first ones may be loading fonts).
    pub slow_strikes: u32,
}

impl LiveUnit {
    fn is_empty(&self) -> bool {
        self.derived.is_none()
            && self.rows.is_none()
            && self.place.is_none()
            && self.counters.is_none()
            && self.probe.is_none()
            && self.over_budget.is_none()
            && self.internal_error.is_none()
            && self.leak.is_none()
            && self.slow_strikes == 0
    }
}

/// The live records of all spans (one mutex in `Shared`; take it for short, non-nested scopes).
#[derive(Debug, Default)]
pub(super) struct LiveUnits(HashMap<ParaId, LiveUnit>);

impl LiveUnits {
    pub fn get(&self, id: ParaId) -> Option<&LiveUnit> {
        self.0.get(&id)
    }

    /// The record of `id`, created empty when there is none.
    pub fn unit(&mut self, id: ParaId) -> &mut LiveUnit {
        self.0.entry(id).or_default()
    }

    /// The probe verdict of `id` for layout `lv`.
    pub fn probe_for(&self, id: ParaId, lv: u64) -> Option<ProbeVerdict> {
        self.get(id)?
            .probe
            .as_ref()
            .filter(|(v, _)| *v == lv)
            .map(|(_, p)| p.clone())
    }

    /// The internal error of `id` recorded for layout `lv`.
    pub fn internal_error_for(&self, id: ParaId, lv: u64) -> Option<String> {
        self.get(id)?
            .internal_error
            .as_ref()
            .filter(|(v, _)| *v == lv)
            .map(|(_, m)| m.clone())
    }

    /// Is `id` compiled on a borrowed context (since the last layout)?
    pub fn is_derived(&self, id: ParaId) -> bool {
        self.get(id).is_some_and(|u| u.derived.is_some())
    }

    /// A layout was installed (version `lv`): the pass's placements and contexts supersede what
    /// the live path guessed, and facts tagged with an older layout no longer apply.
    pub fn on_new_layout(&mut self, lv: u64) {
        for u in self.0.values_mut() {
            u.derived = None;
            u.rows = None;
            u.place = None;
            u.counters = None;
            if u.probe.as_ref().is_some_and(|(v, _)| *v != lv) {
                u.probe = None;
            }
            if u.over_budget
                .is_some_and(|(v, _)| v != lv && v != QUARANTINED)
            {
                u.over_budget = None;
            }
            if u.internal_error.as_ref().is_some_and(|(v, _)| *v != lv) {
                u.internal_error = None;
            }
        }
        self.prune();
    }

    /// The running server was replaced by one with the same preamble and newer bibliography
    /// data: probe verdicts compared that server's output, the rest still holds.
    pub fn on_server_swap(&mut self) {
        for u in self.0.values_mut() {
            u.probe = None;
        }
    }

    /// The preamble changed: the engine restarts, so verdicts, budget marks, quarantines and
    /// leaks about the old engine no longer apply.
    pub fn on_preamble_change(&mut self) {
        for u in self.0.values_mut() {
            u.probe = None;
            u.over_budget = None;
            u.leak = None;
        }
        self.prune();
    }

    fn prune(&mut self) {
        self.0.retain(|_, u| !u.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_rules() {
        let mut l = LiveUnits::default();
        let a = ParaId(1);
        let b = ParaId(2);
        {
            let u = l.unit(a);
            u.derived = Some((b, b, true));
            u.rows = Some(3);
            u.place = Some((1, 2, 3));
            u.probe = Some((5, ProbeVerdict::Verified));
            u.over_budget = Some((5, 9));
            u.internal_error = Some((5, "e".into()));
            u.leak = Some("\\x".into());
            u.slow_strikes = 2;
        }
        l.unit(b).over_budget = Some((QUARANTINED, 0));
        assert!(l.is_derived(a));
        assert!(l.probe_for(a, 5).is_some() && l.probe_for(a, 4).is_none());
        assert_eq!(l.internal_error_for(a, 5).as_deref(), Some("e"));
        // the same layout version installed again keeps the version-tagged facts
        l.on_new_layout(5);
        assert!(!l.is_derived(a) && l.get(a).unwrap().rows.is_none());
        assert!(l.probe_for(a, 5).is_some());
        assert!(l.get(a).unwrap().over_budget.is_some());
        // a newer layout drops them; quarantine, leak and strikes stay
        l.on_new_layout(6);
        let u = l.get(a).unwrap();
        assert!(u.probe.is_none() && u.over_budget.is_none() && u.internal_error.is_none());
        assert!(u.leak.is_some() && u.slow_strikes == 2);
        assert_eq!(l.get(b).unwrap().over_budget, Some((QUARANTINED, 0)));
        // a preamble change clears quarantines and leaks; strikes stay; empty records go
        l.on_preamble_change();
        assert!(l.get(b).is_none(), "empty record pruned");
        assert!(l.get(a).unwrap().leak.is_none() && l.get(a).unwrap().slow_strikes == 2);
    }
}
