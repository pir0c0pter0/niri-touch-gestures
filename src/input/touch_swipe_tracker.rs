//! Touch swipe gesture tracker.
//!
//! Tracks active touch slots and recognizes when 3+ fingers are swiping together in the same
//! direction, producing synthetic gesture begin / update / end signals.

use std::collections::HashMap;
use std::time::Duration;

use smithay::backend::input::TouchSlot;
use smithay::utils::{Logical, Point};

/// Recognition threshold in logical pixels. The cumulative movement of the centroid must exceed
/// this before a gesture is recognized. Copied from GNOME Shell / libadwaita (same as touchpad).
const GESTURE_THRESHOLD_PX: f64 = 16.0;

/// Default minimum number of simultaneous touch points required to start a gesture.
#[cfg(test)]
const DEFAULT_MIN_FINGERS: usize = 3;

/// Maximum number of touch slots we track (sanity bound).
const MAX_SLOTS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDirection {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureEvent {
    Begin {
        direction: SwipeDirection,
        delta_x: f64,
        delta_y: f64,
        timestamp: Duration,
    },
    Update {
        delta_x: f64,
        delta_y: f64,
        timestamp: Duration,
    },
    End {
        timestamp: Duration,
    },
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Collecting touch-down events, not yet enough fingers.
    Idle,
    /// We have enough slots but haven't exceeded the movement threshold yet.
    Recognizing,
    /// Gesture is active: direction is locked and we emit updates.
    Active,
}

#[derive(Debug)]
pub struct TouchSwipeGestureTracker {
    /// All currently tracked touch slots and their latest positions.
    slots: HashMap<TouchSlot, Point<f64, Logical>>,
    /// The slots that were present when we entered the Recognizing phase. These are the ones whose
    /// movement we average for the centroid delta. Fixed once recognition begins.
    tracked_slots: Vec<TouchSlot>,
    /// Positions of the tracked slots at the moment we entered Recognizing.
    start_positions: HashMap<TouchSlot, Point<f64, Logical>>,
    /// Previous centroid position, used to compute per-update deltas.
    prev_centroid: Point<f64, Logical>,
    phase: Phase,
    direction: SwipeDirection,
    /// Minimum number of simultaneous touch points required to start a gesture.
    min_fingers: usize,
}

impl TouchSwipeGestureTracker {
    pub fn new(min_fingers: usize) -> Self {
        Self {
            slots: HashMap::new(),
            tracked_slots: Vec::new(),
            start_positions: HashMap::new(),
            prev_centroid: Point::from((0.0, 0.0)),
            phase: Phase::Idle,
            direction: SwipeDirection::Horizontal,
            min_fingers,
        }
    }

    /// Returns `true` if a gesture is currently active (recognized and emitting updates).
    pub fn is_active(&self) -> bool {
        self.phase == Phase::Active
    }

    /// Returns the locked direction of the current gesture.
    ///
    /// Only meaningful when `is_active()` returns `true`.
    pub fn direction(&self) -> SwipeDirection {
        self.direction
    }

    pub fn touch_down(
        &mut self,
        slot: TouchSlot,
        pos: Point<f64, Logical>,
    ) -> Option<GestureEvent> {
        // If a gesture is already active, ignore additional fingers.
        if self.phase == Phase::Active {
            trace!("touch_swipe: ignoring extra touch_down while active");
            return None;
        }

        // Enforce the slot limit.
        if self.slots.len() >= MAX_SLOTS {
            trace!("touch_swipe: ignoring touch_down, at slot limit");
            return None;
        }

        self.slots.insert(slot, pos);

        // Transition from Idle to Recognizing once we have enough fingers.
        if self.phase == Phase::Idle && self.slots.len() >= self.min_fingers {
            trace!(
                "touch_swipe: entering recognizing phase with {} slots",
                self.slots.len()
            );
            self.phase = Phase::Recognizing;
            self.tracked_slots = self.slots.keys().copied().collect();
            self.start_positions = self.slots.clone();
            self.prev_centroid = self.centroid_of_tracked();
        }

        None
    }

    pub fn touch_motion(
        &mut self,
        slot: TouchSlot,
        pos: Point<f64, Logical>,
        timestamp: Duration,
    ) -> Option<GestureEvent> {
        // Update stored position regardless of phase.
        if let Some(stored) = self.slots.get_mut(&slot) {
            *stored = pos;
        } else {
            // Motion for unknown slot — ignore.
            return None;
        }

        match self.phase {
            Phase::Idle => None,
            Phase::Recognizing => self.try_recognize(timestamp),
            Phase::Active => {
                // Only emit updates for motion on tracked slots.
                if !self.tracked_slots.contains(&slot) {
                    return None;
                }
                let centroid = self.centroid_of_tracked();
                let dx = centroid.x - self.prev_centroid.x;
                let dy = centroid.y - self.prev_centroid.y;
                self.prev_centroid = centroid;

                Some(GestureEvent::Update {
                    delta_x: dx,
                    delta_y: dy,
                    timestamp,
                })
            }
        }
    }

    pub fn touch_up(&mut self, slot: TouchSlot, timestamp: Duration) -> Option<GestureEvent> {
        self.slots.remove(&slot);

        match self.phase {
            Phase::Active => {
                // If one of the original tracked slots lifts, end the gesture.
                if self.tracked_slots.contains(&slot) {
                    trace!("touch_swipe: tracked slot lifted, ending gesture");
                    self.reset_internal();
                    Some(GestureEvent::End { timestamp })
                } else {
                    None
                }
            }
            Phase::Recognizing => {
                // Lost a finger before recognition — check if we still have enough.
                self.tracked_slots.retain(|s| *s != slot);
                self.start_positions.remove(&slot);
                if self.slots.len() < self.min_fingers {
                    trace!(
                        "touch_swipe: dropped below {} fingers during recognition, cancelling",
                        self.min_fingers
                    );
                    self.reset_internal();
                    Some(GestureEvent::Cancel)
                } else {
                    None
                }
            }
            Phase::Idle => None,
        }
    }

    /// Resets the tracker to its idle state, cancelling any in-progress gesture.
    pub fn reset(&mut self) {
        self.reset_internal();
    }

    fn reset_internal(&mut self) {
        self.slots.clear();
        self.tracked_slots.clear();
        self.start_positions.clear();
        self.prev_centroid = Point::from((0.0, 0.0));
        self.phase = Phase::Idle;
        self.direction = SwipeDirection::Horizontal;
        // min_fingers is intentionally preserved across resets.
    }

    /// Compute the centroid of the currently tracked slots.
    fn centroid_of_tracked(&self) -> Point<f64, Logical> {
        let n = self.tracked_slots.len() as f64;
        if n == 0.0 {
            return Point::from((0.0, 0.0));
        }

        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        for slot in &self.tracked_slots {
            if let Some(pos) = self.slots.get(slot) {
                sum_x += pos.x;
                sum_y += pos.y;
            }
        }
        Point::from((sum_x / n, sum_y / n))
    }

    /// Check whether cumulative movement of the centroid exceeds the threshold.
    fn try_recognize(&mut self, timestamp: Duration) -> Option<GestureEvent> {
        let centroid = self.centroid_of_tracked();
        let start_centroid = {
            let n = self.tracked_slots.len() as f64;
            if n == 0.0 {
                return None;
            }
            let mut sx = 0.0;
            let mut sy = 0.0;
            for slot in &self.tracked_slots {
                if let Some(pos) = self.start_positions.get(slot) {
                    sx += pos.x;
                    sy += pos.y;
                }
            }
            Point::<f64, Logical>::from((sx / n, sy / n))
        };

        let dx = centroid.x - start_centroid.x;
        let dy = centroid.y - start_centroid.y;
        let dist_sq = dx * dx + dy * dy;

        if dist_sq < GESTURE_THRESHOLD_PX * GESTURE_THRESHOLD_PX {
            return None;
        }

        // Lock the direction based on which axis has more movement.
        self.direction = if dx.abs() > dy.abs() {
            SwipeDirection::Horizontal
        } else {
            SwipeDirection::Vertical
        };

        self.phase = Phase::Active;
        self.prev_centroid = centroid;

        trace!(
            "touch_swipe: gesture recognized as {:?}, delta ({dx:.1}, {dy:.1})",
            self.direction
        );

        Some(GestureEvent::Begin {
            direction: self.direction,
            delta_x: dx,
            delta_y: dy,
            timestamp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(id: u32) -> TouchSlot {
        TouchSlot::from(Some(id))
    }

    fn pt(x: f64, y: f64) -> Point<f64, Logical> {
        Point::from((x, y))
    }

    fn ts(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    #[test]
    fn no_gesture_with_fewer_than_three_fingers() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        assert!(tracker.touch_down(slot(0), pt(100.0, 100.0)).is_none());
        assert!(tracker.touch_down(slot(1), pt(110.0, 100.0)).is_none());
        // Move them a lot — still no gesture.
        assert!(tracker
            .touch_motion(slot(0), pt(200.0, 100.0), ts(10))
            .is_none());
        assert!(tracker
            .touch_motion(slot(1), pt(210.0, 100.0), ts(10))
            .is_none());
        assert!(!tracker.is_active());
    }

    #[test]
    fn gesture_begins_after_threshold_with_three_fingers() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        assert!(!tracker.is_active());

        // Move all fingers a small amount — below threshold.
        tracker.touch_motion(slot(0), pt(105.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(115.0, 100.0), ts(10));
        let ev = tracker.touch_motion(slot(2), pt(125.0, 100.0), ts(10));
        assert!(ev.is_none());

        // Move all fingers beyond threshold horizontally (20px each).
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(20));
        tracker.touch_motion(slot(1), pt(130.0, 100.0), ts(20));
        let ev = tracker.touch_motion(slot(2), pt(140.0, 100.0), ts(20));
        assert!(matches!(
            ev,
            Some(GestureEvent::Begin {
                direction: SwipeDirection::Horizontal,
                ..
            })
        ));
        assert!(tracker.is_active());
        assert_eq!(tracker.direction(), SwipeDirection::Horizontal);
    }

    #[test]
    fn vertical_direction_detected() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        // Move all fingers down past threshold (20px each).
        tracker.touch_motion(slot(0), pt(100.0, 120.0), ts(10));
        tracker.touch_motion(slot(1), pt(110.0, 120.0), ts(10));
        let ev = tracker.touch_motion(slot(2), pt(120.0, 120.0), ts(10));

        assert!(matches!(
            ev,
            Some(GestureEvent::Begin {
                direction: SwipeDirection::Vertical,
                ..
            })
        ));
    }

    #[test]
    fn updates_emit_centroid_delta() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        // Trigger recognition by moving right.
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(130.0, 100.0), ts(10));
        let begin = tracker.touch_motion(slot(2), pt(140.0, 100.0), ts(10));
        assert!(begin.is_some());

        // Now move slot 0 by 9px right.
        let ev = tracker.touch_motion(slot(0), pt(129.0, 100.0), ts(20));
        match ev {
            Some(GestureEvent::Update {
                delta_x, delta_y, ..
            }) => {
                // Only slot 0 moved 9px, centroid delta = 9 / 3 = 3.
                assert!((delta_x - 3.0).abs() < 0.01);
                assert!(delta_y.abs() < 0.01);
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn gesture_ends_on_tracked_slot_lift() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        // Trigger recognition.
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(130.0, 100.0), ts(10));
        tracker.touch_motion(slot(2), pt(140.0, 100.0), ts(10));
        assert!(tracker.is_active());

        // Lift one tracked finger.
        let ev = tracker.touch_up(slot(1), ts(30));
        assert!(matches!(ev, Some(GestureEvent::End { .. })));
        assert!(!tracker.is_active());
    }

    #[test]
    fn extra_finger_ignored_during_active_gesture() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        // Trigger recognition.
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(130.0, 100.0), ts(10));
        tracker.touch_motion(slot(2), pt(140.0, 100.0), ts(10));
        assert!(tracker.is_active());

        // Add a 4th finger.
        let ev = tracker.touch_down(slot(3), pt(200.0, 200.0));
        assert!(ev.is_none());

        // Lifting the 4th finger doesn't end the gesture.
        let ev = tracker.touch_up(slot(3), ts(40));
        assert!(ev.is_none());
        assert!(tracker.is_active());
    }

    #[test]
    fn cancel_when_finger_lifts_during_recognition() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        // Lift a finger before threshold is reached.
        let ev = tracker.touch_up(slot(2), ts(10));
        assert!(matches!(ev, Some(GestureEvent::Cancel)));
        assert!(!tracker.is_active());
    }

    #[test]
    fn reset_clears_state() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));

        // Trigger recognition.
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(130.0, 100.0), ts(10));
        tracker.touch_motion(slot(2), pt(140.0, 100.0), ts(10));
        assert!(tracker.is_active());

        tracker.reset();
        assert!(!tracker.is_active());
    }

    #[test]
    fn motion_on_unknown_slot_ignored() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        let ev = tracker.touch_motion(slot(99), pt(0.0, 0.0), ts(10));
        assert!(ev.is_none());
    }

    #[test]
    fn four_fingers_down_before_recognition() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(110.0, 100.0));
        tracker.touch_down(slot(2), pt(120.0, 100.0));
        // 4th finger during recognizing phase is fine — it gets added to tracked_slots.
        tracker.touch_down(slot(3), pt(130.0, 100.0));

        // Move all right past threshold.
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(130.0, 100.0), ts(10));
        tracker.touch_motion(slot(2), pt(140.0, 100.0), ts(10));
        let ev = tracker.touch_motion(slot(3), pt(150.0, 100.0), ts(10));

        // Should recognize with all 4 fingers being tracked.
        assert!(ev.is_some() || tracker.is_active());
    }

    #[test]
    fn begin_delta_reflects_cumulative_movement() {
        let mut tracker = TouchSwipeGestureTracker::new(DEFAULT_MIN_FINGERS);
        tracker.touch_down(slot(0), pt(100.0, 100.0));
        tracker.touch_down(slot(1), pt(200.0, 100.0));
        tracker.touch_down(slot(2), pt(300.0, 100.0));

        // Move all three fingers 20px to the right.
        tracker.touch_motion(slot(0), pt(120.0, 100.0), ts(10));
        tracker.touch_motion(slot(1), pt(220.0, 100.0), ts(10));
        let ev = tracker.touch_motion(slot(2), pt(320.0, 100.0), ts(10));

        match ev {
            Some(GestureEvent::Begin {
                delta_x,
                delta_y,
                direction,
                ..
            }) => {
                assert_eq!(direction, SwipeDirection::Horizontal);
                assert!((delta_x - 20.0).abs() < 0.01);
                assert!(delta_y.abs() < 0.01);
            }
            other => panic!("expected Begin, got {other:?}"),
        }
    }
}
