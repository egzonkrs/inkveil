//! How open tabs are arranged: pinned tabs, pairs shown in a split view, and where a dragged tab
//! lands. Plain logic on ids, so every rule can be tested without a window.
//!
//! Two rules always hold for an order of tabs: pinned tabs come first, and the two tabs of a split
//! sit next to each other, `first` before `second`. Both tabs of a split are pinned or neither is.

use serde::{Deserialize, Serialize};

/// Half and half, in thousandths of the space.
pub const EVEN_RATIO: u16 = 500;
const MIN_RATIO: u16 = 200;
const MAX_RATIO: u16 = 800;

/// Two tabs shown at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split<Id> {
    /// Shown on the left, or on top when stacked.
    pub first: Id,
    pub second: Id,
    /// One above the other instead of side by side.
    pub stacked: bool,
    /// The share of the space `first` gets, in thousandths.
    pub ratio: u16,
}

impl<Id: Copy + PartialEq> Split<Id> {
    pub fn new(first: Id, second: Id) -> Self {
        Self {
            first,
            second,
            stacked: false,
            ratio: EVEN_RATIO,
        }
    }

    pub fn has(&self, id: Id) -> bool {
        self.first == id || self.second == id
    }

    /// The other tab of the pair.
    pub fn other(&self, id: Id) -> Id {
        if self.first == id {
            self.second
        } else {
            self.first
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabLayout<Id> {
    pub pinned: Vec<Id>,
    pub splits: Vec<Split<Id>>,
}

impl<Id> Default for TabLayout<Id> {
    fn default() -> Self {
        Self {
            pinned: Vec::new(),
            splits: Vec::new(),
        }
    }
}

impl<Id: Copy + PartialEq> TabLayout<Id> {
    pub fn split_of(&self, id: Id) -> Option<&Split<Id>> {
        self.splits.iter().find(|s| s.has(id))
    }

    fn split_of_mut(&mut self, id: Id) -> Option<&mut Split<Id>> {
        self.splits.iter_mut().find(|s| s.has(id))
    }

    pub fn is_pinned(&self, id: Id) -> bool {
        self.pinned.contains(&id)
    }

    /// The tabs that move together with `id`, in tab order.
    pub fn members(&self, id: Id) -> Vec<Id> {
        match self.split_of(id) {
            Some(split) => vec![split.first, split.second],
            None => vec![id],
        }
    }

    /// Pinned tabs first; the order within both groups stays.
    fn pinned_first(&self, order: &[Id]) -> Vec<Id> {
        let (mut pinned, rest): (Vec<Id>, Vec<Id>) =
            order.iter().copied().partition(|id| self.is_pinned(*id));
        pinned.extend(rest);
        pinned
    }

    /// Where `dragged` would really land when dropped at `slot` (an index between tabs, from 0 to
    /// the number of tabs): it stays with the pinned or unpinned tabs and never lands between the
    /// two tabs of a split.
    pub fn drop_slot(&self, order: &[Id], dragged: Id, slot: usize) -> usize {
        let mut slot = slot.min(order.len());
        if slot > 0
            && slot < order.len()
            && self
                .split_of(order[slot])
                .is_some_and(|s| s.second == order[slot] && s.first == order[slot - 1])
        {
            let from = order.iter().position(|id| *id == dragged).unwrap_or(0);
            if from < slot {
                slot += 1;
            } else {
                slot -= 1;
            }
        }
        let pinned = order.iter().filter(|id| self.is_pinned(**id)).count();
        if self.is_pinned(dragged) {
            slot.min(pinned)
        } else {
            slot.max(pinned)
        }
    }

    /// The order after dropping `dragged` at `slot`. A tab in a split brings its partner along.
    pub fn move_tab(&self, order: &[Id], dragged: Id, slot: usize) -> Vec<Id> {
        let slot = self.drop_slot(order, dragged, slot);
        let unit = self.members(dragged);
        let at = order[..slot].iter().filter(|id| !unit.contains(id)).count();
        let mut moved: Vec<Id> = order
            .iter()
            .copied()
            .filter(|id| !unit.contains(id))
            .collect();
        moved.splice(at..at, unit);
        moved
    }

    /// Shows `first` and `second` together, where the earlier of the two was, and returns the new
    /// order. Tabs that are already in a split stay as they are.
    pub fn split(&mut self, order: &[Id], first: Id, second: Id) -> Vec<Id> {
        let position = |target: Id| order.iter().position(|id| *id == target);
        let (Some(a), Some(b)) = (position(first), position(second)) else {
            return order.to_vec();
        };
        if first == second || self.split_of(first).is_some() || self.split_of(second).is_some() {
            return order.to_vec();
        }
        self.splits.push(Split::new(first, second));
        if self.is_pinned(first) != self.is_pinned(second) {
            let unpinned = if self.is_pinned(first) { second } else { first };
            self.pinned.push(unpinned);
        }
        let mut joined: Vec<Id> = order
            .iter()
            .copied()
            .filter(|id| *id != first && *id != second)
            .collect();
        let at = a.min(b);
        joined.splice(at..at, [first, second]);
        self.pinned_first(&joined)
    }

    /// Puts back a stored split, keeping its orientation and size.
    pub fn restore_split(&mut self, order: &[Id], stored: Split<Id>) -> Vec<Id> {
        let joined = self.split(order, stored.first, stored.second);
        if let Some(split) = self.split_of_mut(stored.first) {
            split.stacked = stored.stacked;
            split.ratio = stored.ratio.clamp(MIN_RATIO, MAX_RATIO);
        }
        joined
    }

    /// Ends the split `id` is in; both tabs stay open.
    pub fn unsplit(&mut self, id: Id) {
        self.splits.retain(|s| !s.has(id));
    }

    /// Exchanges the two sides of the split `id` is in.
    pub fn swap(&mut self, order: &[Id], id: Id) -> Vec<Id> {
        let mut swapped = order.to_vec();
        let Some(split) = self.split_of_mut(id) else {
            return swapped;
        };
        std::mem::swap(&mut split.first, &mut split.second);
        let (first, second) = (split.first, split.second);
        for tab in &mut swapped {
            if *tab == first {
                *tab = second;
            } else if *tab == second {
                *tab = first;
            }
        }
        swapped
    }

    pub fn toggle_stacked(&mut self, id: Id) {
        if let Some(split) = self.split_of_mut(id) {
            split.stacked = !split.stacked;
        }
    }

    /// Sets how much of the space the first tab gets, within limits that keep both usable.
    pub fn set_ratio(&mut self, id: Id, ratio: u16) {
        if let Some(split) = self.split_of_mut(id) {
            split.ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
        }
    }

    /// Pins or unpins `id` and its partner, and returns the new order.
    pub fn toggle_pin(&mut self, order: &[Id], id: Id) -> Vec<Id> {
        let members = self.members(id);
        if self.is_pinned(id) {
            self.pinned.retain(|p| !members.contains(p));
        } else {
            self.pinned.extend(members);
        }
        self.pinned_first(order)
    }

    /// Forgets a tab that closed. Its partner becomes an ordinary tab but stays pinned if it was.
    pub fn remove(&mut self, id: Id) {
        self.pinned.retain(|p| *p != id);
        self.unsplit(id);
    }

    /// The tabs "Close other tabs" closes: all but pinned ones, `id` and its partner.
    pub fn others(&self, order: &[Id], id: Id) -> Vec<Id> {
        let keep = self.members(id);
        order
            .iter()
            .copied()
            .filter(|tab| !keep.contains(tab) && !self.is_pinned(*tab))
            .collect()
    }

    /// The tabs "Close tabs to the right" closes.
    pub fn to_the_right(&self, order: &[Id], id: Id) -> Vec<Id> {
        let last = self.members(id).into_iter().last().unwrap_or(id);
        let Some(index) = order.iter().position(|tab| *tab == last) else {
            return Vec::new();
        };
        order[index + 1..]
            .iter()
            .copied()
            .filter(|tab| !self.is_pinned(*tab))
            .collect()
    }

    /// The tab to pair with `id` when none was chosen: the nearest free one, looking right first.
    pub fn nearest_free(&self, order: &[Id], id: Id) -> Option<Id> {
        let index = order.iter().position(|tab| *tab == id)?;
        let free = |tab: &&Id| self.split_of(**tab).is_none();
        order[index + 1..]
            .iter()
            .find(free)
            .or_else(|| order[..index].iter().rev().find(free))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> TabLayout<u32> {
        TabLayout::default()
    }

    #[test]
    fn moves_a_tab_to_either_side_of_another() {
        let order = [1, 2, 3, 4];
        assert_eq!(layout().move_tab(&order, 1, 3), [2, 3, 1, 4]);
        assert_eq!(layout().move_tab(&order, 4, 0), [4, 1, 2, 3]);
        assert_eq!(layout().move_tab(&order, 2, 4), [1, 3, 4, 2]);
        // Dropping a tab next to itself changes nothing.
        assert_eq!(layout().move_tab(&order, 2, 1), order);
        assert_eq!(layout().move_tab(&order, 2, 2), order);
    }

    #[test]
    fn a_split_moves_as_one_and_cannot_be_separated() {
        let mut l = layout();
        let order = l.split(&[1, 2, 3, 4, 5], 2, 3);
        assert_eq!(order, [1, 2, 3, 4, 5]);
        // Either half of the pair drags both.
        assert_eq!(l.move_tab(&order, 3, 5), [1, 4, 5, 2, 3]);
        assert_eq!(l.move_tab(&order, 2, 0), [2, 3, 1, 4, 5]);
        // A tab dropped between the two lands beside the pair, coming from either side.
        assert_eq!(l.move_tab(&order, 1, 2), [2, 3, 1, 4, 5]);
        assert_eq!(l.move_tab(&order, 5, 2), [1, 5, 2, 3, 4]);
    }

    #[test]
    fn pinned_and_other_tabs_stay_in_their_own_groups() {
        let mut l = layout();
        let order = l.toggle_pin(&[1, 2, 3, 4], 3);
        assert_eq!(order, [3, 1, 2, 4]);
        let order = l.toggle_pin(&order, 4);
        assert_eq!(order, [3, 4, 1, 2]);
        assert_eq!(l.move_tab(&order, 1, 0), [3, 4, 1, 2]);
        assert_eq!(l.move_tab(&order, 3, 4), [4, 3, 1, 2]);
        let order = l.toggle_pin(&order, 3);
        assert_eq!(order, [4, 3, 1, 2]);
        assert!(!l.is_pinned(3));
    }

    #[test]
    fn splitting_joins_two_tabs_at_the_earlier_place() {
        let mut l = layout();
        assert_eq!(l.split(&[1, 2, 3, 4], 4, 2), [1, 4, 2, 3]);
        let split = l.split_of(2).copied().unwrap();
        assert_eq!((split.first, split.second), (4, 2));
        assert_eq!(split.other(4), 2);
    }

    #[test]
    fn a_tab_is_in_one_split_at_most() {
        let mut l = layout();
        let order = l.split(&[1, 2, 3], 1, 2);
        assert_eq!(l.split(&order, 2, 3), order);
        assert_eq!(l.split(&order, 3, 3), order);
        assert_eq!(l.split(&order, 3, 9), order);
        assert_eq!(l.splits.len(), 1);
    }

    #[test]
    fn splitting_with_a_pinned_tab_pins_both() {
        let mut l = layout();
        let order = l.toggle_pin(&[1, 2, 3], 1);
        let order = l.split(&order, 3, 1);
        assert_eq!(order, [3, 1, 2]);
        assert!(l.is_pinned(3));
        let order = l.toggle_pin(&order, 1);
        assert_eq!(order, [3, 1, 2]);
        assert_eq!(l.pinned, Vec::<u32>::new());
    }

    #[test]
    fn swapping_exchanges_the_sides_and_the_tabs() {
        let mut l = layout();
        let order = l.split(&[1, 2, 3, 4], 2, 3);
        assert_eq!(l.swap(&order, 3), [1, 3, 2, 4]);
        assert_eq!(l.split_of(2).unwrap().first, 3);
        assert_eq!(l.swap(&[1, 2], 7), [1, 2]);
    }

    #[test]
    fn stacked_and_size_stay_within_limits() {
        let mut l = layout();
        l.split(&[1, 2], 1, 2);
        l.toggle_stacked(2);
        assert!(l.split_of(1).unwrap().stacked);
        l.set_ratio(1, 5);
        assert_eq!(l.split_of(1).unwrap().ratio, MIN_RATIO);
        l.set_ratio(1, 999);
        assert_eq!(l.split_of(1).unwrap().ratio, MAX_RATIO);
    }

    #[test]
    fn closing_one_side_ends_the_split_and_keeps_the_other_tab() {
        let mut l = layout();
        let order = l.toggle_pin(&[1, 2], 1);
        l.split(&order, 1, 2);
        l.remove(2);
        assert!(l.split_of(1).is_none());
        assert!(l.is_pinned(1));
        assert!(!l.is_pinned(2));
    }

    #[test]
    fn unsplitting_keeps_both_tabs_where_they_are() {
        let mut l = layout();
        let order = l.split(&[1, 2, 3], 2, 3);
        l.unsplit(3);
        assert_eq!(l.splits.len(), 0);
        assert_eq!(order, [1, 2, 3]);
    }

    #[test]
    fn restoring_keeps_the_arrangement_of_a_split() {
        let mut l = layout();
        let stored = Split {
            first: 3,
            second: 1,
            stacked: true,
            ratio: 650,
        };
        assert_eq!(l.restore_split(&[1, 2, 3], stored), [3, 1, 2]);
        assert_eq!(l.split_of(1).copied(), Some(stored));
    }

    #[test]
    fn close_commands_spare_pinned_tabs_and_the_partner() {
        let mut l = layout();
        let order = l.toggle_pin(&[1, 2, 3, 4, 5], 1);
        let order = l.split(&order, 3, 4);
        assert_eq!(l.others(&order, 3), [2, 5]);
        assert_eq!(l.others(&order, 2), [3, 4, 5]);
        assert_eq!(l.to_the_right(&order, 3), [5]);
        assert_eq!(l.to_the_right(&order, 1), [2, 3, 4, 5]);
        assert_eq!(l.to_the_right(&order, 5), Vec::<u32>::new());
    }

    #[test]
    fn nearest_free_tab_looks_right_then_left_and_skips_splits() {
        let mut l = layout();
        let order = l.split(&[1, 2, 3, 4], 3, 4);
        assert_eq!(l.nearest_free(&order, 1), Some(2));
        assert_eq!(l.nearest_free(&order, 2), Some(1));
        let order = l.split(&order, 1, 2);
        assert_eq!(l.nearest_free(&order, 1), None);
    }
}
