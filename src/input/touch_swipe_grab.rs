use std::time::Duration;

use smithay::input::touch::{
    DownEvent, GrabStartData as TouchGrabStartData, MotionEvent, OrientationEvent, ShapeEvent,
    TouchGrab, TouchInnerHandle, UpEvent,
};
use smithay::input::SeatHandler;
use smithay::output::Output;
use smithay::utils::{Logical, Point, Serial};

use crate::input::touch_swipe_tracker::{GestureEvent, SwipeDirection, TouchSwipeGestureTracker};
use crate::niri::State;

/// Gesture state for a 3-finger touchscreen swipe outside the overview.
///
/// Feeds deltas into the same `Layout::workspace_switch_gesture_*` and `view_offset_gesture_*`
/// pipeline that the touchpad path uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GestureState {
    /// The tracker is still collecting fingers / hasn't exceeded the movement threshold.
    Recognizing,
    /// Horizontal swipe — drives `view_offset_gesture_*`.
    ViewOffset,
    /// Vertical swipe — drives `workspace_switch_gesture_*`.
    WorkspaceSwitch,
}

pub struct TouchSwipeGrab {
    start_data: TouchGrabStartData<State>,
    output: Output,
    tracker: TouchSwipeGestureTracker,
    gesture: GestureState,
}

impl TouchSwipeGrab {
    pub fn new(
        start_data: TouchGrabStartData<State>,
        output: Output,
        tracker: TouchSwipeGestureTracker,
    ) -> Self {
        let gesture = if tracker.is_active() {
            match tracker.direction() {
                SwipeDirection::Horizontal => GestureState::ViewOffset,
                SwipeDirection::Vertical => GestureState::WorkspaceSwitch,
            }
        } else {
            GestureState::Recognizing
        };

        Self {
            start_data,
            output,
            tracker,
            gesture,
        }
    }

    fn handle_gesture_event(&mut self, event: Option<GestureEvent>, data: &mut State) -> bool {
        let Some(event) = event else {
            return false;
        };

        let layout = &mut data.niri.layout;

        match event {
            GestureEvent::Begin {
                direction,
                delta_x,
                delta_y,
                timestamp,
            } => {
                match direction {
                    SwipeDirection::Horizontal => {
                        layout.view_offset_gesture_begin(&self.output, None, false);
                        self.gesture = GestureState::ViewOffset;
                        layout.view_offset_gesture_update(-delta_x, timestamp, false);
                    }
                    SwipeDirection::Vertical => {
                        layout.workspace_switch_gesture_begin(&self.output, false);
                        self.gesture = GestureState::WorkspaceSwitch;
                        layout.workspace_switch_gesture_update(-delta_y, timestamp, false);
                    }
                }
                data.niri.queue_redraw_all();
            }
            GestureEvent::Update {
                delta_x,
                delta_y,
                timestamp,
            } => {
                let ongoing = match self.gesture {
                    GestureState::Recognizing => return false,
                    GestureState::ViewOffset => layout
                        .view_offset_gesture_update(-delta_x, timestamp, false)
                        .is_some(),
                    GestureState::WorkspaceSwitch => layout
                        .workspace_switch_gesture_update(-delta_y, timestamp, false)
                        .is_some(),
                };

                if ongoing {
                    data.niri.queue_redraw_all();
                } else {
                    // Gesture pipeline says stop.
                    return true;
                }
            }
            GestureEvent::End { .. } => {
                match self.gesture {
                    GestureState::Recognizing => {}
                    GestureState::ViewOffset => {
                        layout.view_offset_gesture_end(Some(false));
                    }
                    GestureState::WorkspaceSwitch => {
                        layout.workspace_switch_gesture_end(Some(false));
                    }
                }
                data.niri.queue_redraw_all();
                return true;
            }
            GestureEvent::Cancel => {
                match self.gesture {
                    GestureState::Recognizing => {}
                    GestureState::ViewOffset => {
                        layout.view_offset_gesture_end(None);
                    }
                    GestureState::WorkspaceSwitch => {
                        layout.workspace_switch_gesture_end(None);
                    }
                }
                data.niri.queue_redraw_all();
                return true;
            }
        }

        false
    }

    fn on_ungrab(&mut self, data: &mut State) {
        let layout = &mut data.niri.layout;
        match self.gesture {
            GestureState::Recognizing => {}
            GestureState::ViewOffset => {
                layout.view_offset_gesture_end(Some(false));
            }
            GestureState::WorkspaceSwitch => {
                layout.workspace_switch_gesture_end(Some(false));
            }
        }
        data.niri.queue_redraw_all();
    }
}

impl TouchGrab<State> for TouchSwipeGrab {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &DownEvent,
        seq: Serial,
    ) {
        handle.down(data, None, event, seq);

        let ev = self.tracker.touch_down(event.slot, event.location);
        if self.handle_gesture_event(ev, data) {
            handle.unset_grab(self, data);
        }
    }

    fn up(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &UpEvent,
        seq: Serial,
    ) {
        handle.up(data, event, seq);

        let timestamp = Duration::from_millis(u64::from(event.time));
        let ev = self.tracker.touch_up(event.slot, timestamp);
        if self.handle_gesture_event(ev, data) {
            handle.unset_grab(self, data);
        }
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
        seq: Serial,
    ) {
        handle.motion(data, None, event, seq);

        let timestamp = Duration::from_millis(u64::from(event.time));
        let ev = self
            .tracker
            .touch_motion(event.slot, event.location, timestamp);
        if self.handle_gesture_event(ev, data) {
            handle.unset_grab(self, data);
        }
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, seq: Serial) {
        handle.frame(data, seq);
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, seq: Serial) {
        handle.cancel(data, seq);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &ShapeEvent,
        seq: Serial,
    ) {
        handle.shape(data, event, seq);
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &OrientationEvent,
        seq: Serial,
    ) {
        handle.orientation(data, event, seq);
    }

    fn start_data(&self) -> &TouchGrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, data: &mut State) {
        self.on_ungrab(data);
    }
}
