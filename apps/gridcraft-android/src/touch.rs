//! Finger input for a touch screen.
//!
//! On Android every pointer arrives as a touch, and egui-winit turns the first finger into a
//! held-down mouse button. A swipe over the page would then select text instead of scrolling,
//! and a finger resting on a button would press it at once. This module replaces those emulated
//! mouse events with gestures:
//!
//! - **tap**: a click where the finger lifted (two quick taps: a double-click, as egui counts them);
//! - **swipe**: scrolling (as a touchpad's wheel events, so every scroll area and the page follow),
//!   with a short coast after a flick;
//! - **long press, then drag**: the mouse button held down, so dragging selects cells (the raw
//!   touches are held back meanwhile: the grid pans the sheet on any touch drag, which here would
//!   undo the selection);
//! - **two fingers**: egui's pinch zoom (the touch events pass through untouched); lifting both
//!   without moving is a right-click, for the context menu.
//!
//! Pure input rewriting over [`egui::RawInput`], so it is tested on the desktop.

use egui::{Event, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, RawInput, TouchId, TouchPhase, Vec2};

/// How far (points) a finger may wander and still be a tap or a long press.
const SLOP: f32 = 10.0;
/// How long (seconds) a finger rests before it becomes a held mouse button.
const LONG_PRESS: f64 = 0.45;
/// Two fingers lifted within this time (seconds) of the second landing are a right-click.
const TWO_FINGER_TAP: f64 = 0.35;
/// A flick slower than this (points per second) doesn't coast.
const MIN_FLING: f32 = 120.0;
/// Fraction of the coasting speed kept after one second.
const FLING_DECAY_PER_SECOND: f32 = 0.02;
/// Fastest coast (points per second), so one hard flick doesn't jump pages.
const MAX_FLING: f32 = 6000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// No finger down.
    Idle,
    /// One finger down, not yet a swipe or a long press.
    Pending { id: TouchId, start: Pos2, since: f64, last: Pos2 },
    /// One finger swiping: scrolling.
    Scrolling { id: TouchId, last: Pos2, last_time: f64, velocity: Vec2 },
    /// One finger held: the primary button is down and moves drag (select).
    Pressing { id: TouchId },
    /// Two or more fingers: pinch zoom, or a right-click when they lift without moving.
    Multi { since: f64, first: Pos2, moved: bool },
}

/// Gesture state carried between frames.
#[derive(Debug)]
pub struct Touches {
    state: State,
    /// Fingers currently down.
    down: Vec<(TouchId, Pos2)>,
    /// Events held back for the next frame, so a press and its release land in separate frames.
    next: Vec<Event>,
    /// A flick still coasting: its velocity (points per second) and the time of the last step.
    fling: Option<(Vec2, f64)>,
    /// Time of the previous frame's input.
    last_time: f64,
    /// The modifier keys as the last key event left them (a hardware keyboard's Shift extends a
    /// selection with a tap).
    modifiers: Modifiers,
    /// The touch device the fingers are on (for the cancel that ends a held finger's touch).
    device: egui::TouchDeviceId,
}

impl Default for Touches {
    fn default() -> Self {
        Self {
            state: State::Idle,
            down: Vec::new(),
            next: Vec::new(),
            fling: None,
            last_time: 0.0,
            modifiers: Modifiers::NONE,
            device: egui::TouchDeviceId(0),
        }
    }
}

impl Touches {
    /// Rewrite one frame's input. Returns whether another frame is needed soon (a long press
    /// may be due, events wait for the next frame, or a flick is coasting).
    pub fn process(&mut self, raw: &mut RawInput) -> bool {
        let now = raw.time.unwrap_or(self.last_time);
        let dt = (now - self.last_time).clamp(0.0, 0.1) as f32;
        self.last_time = now;
        let incoming = std::mem::take(&mut raw.events);
        if let Some(m) = incoming.iter().rev().find_map(|e| match e {
            Event::Key { modifiers, .. } => Some(*modifiers),
            _ => None,
        }) {
            self.modifiers = m;
        }
        let modifiers = self.modifiers;
        let mut out: Vec<Event> = std::mem::take(&mut self.next);

        // A coasting flick moves on a step before anything new happens.
        if let Some((velocity, _)) = self.fling {
            let step = velocity * dt;
            let decayed = velocity * FLING_DECAY_PER_SECOND.powf(dt);
            if decayed.length() < MIN_FLING / 2.0 {
                self.fling = None;
                out.push(wheel(Vec2::ZERO, TouchPhase::End, modifiers));
            } else {
                self.fling = Some((decayed, now));
                out.push(wheel(step, TouchPhase::Move, modifiers));
            }
        }

        let has_touch = incoming.iter().any(|e| matches!(e, Event::Touch { .. }));
        for event in incoming {
            match event {
                Event::Touch { device_id, id, phase, pos, .. } => {
                    self.device = device_id;
                    // While one finger holds the button for a drag-select, egui mustn't see it as
                    // a touch (the grid would pan). Two fingers and everything else go through,
                    // for egui's pinch zoom.
                    if !matches!(self.state, State::Pressing { id: p } if p == id) {
                        out.push(event.clone());
                    }
                    self.touch(id, phase, pos, now, modifiers, &mut out);
                }
                // egui-winit's mouse emulation of the first finger: replaced by the gestures.
                // A real mouse (where the platform reports one) never comes with touches.
                Event::PointerMoved(_) | Event::PointerButton { button: PointerButton::Primary, .. } | Event::PointerGone
                    if has_touch || self.state != State::Idle => {}
                other => out.push(other),
            }
        }

        // A finger resting long enough becomes a held button.
        if let State::Pending { id, start, since, .. } = self.state
            && now - since >= LONG_PRESS
        {
            // egui saw this finger land: end its touch there, so it doesn't stay "down" while the
            // rest of the drag reaches egui as a mouse.
            out.push(Event::Touch { device_id: self.device, id, phase: TouchPhase::Cancel, pos: start, force: None });
            out.push(Event::PointerMoved(start));
            out.push(button(start, PointerButton::Primary, true, modifiers));
            self.state = State::Pressing { id };
        }

        raw.events = out;
        !self.next.is_empty() || self.fling.is_some() || matches!(self.state, State::Pending { .. })
    }

    fn touch(&mut self, id: TouchId, phase: TouchPhase, pos: Pos2, now: f64, modifiers: Modifiers, out: &mut Vec<Event>) {
        match phase {
            TouchPhase::Start => {
                self.down.retain(|(d, _)| *d != id);
                self.down.push((id, pos));
                // A new finger stops a coasting flick where it is.
                if self.fling.take().is_some() {
                    out.push(wheel(Vec2::ZERO, TouchPhase::End, modifiers));
                }
                match self.state {
                    State::Idle => {
                        // Hover there, so a swipe scrolls what is under the finger.
                        out.push(Event::PointerMoved(pos));
                        self.state = State::Pending { id, start: pos, since: now, last: pos };
                    }
                    State::Pressing { .. } => {
                        if let Some(p) = self.down.first().map(|(_, p)| *p) {
                            out.push(button(p, PointerButton::Primary, false, modifiers));
                        }
                        self.state = State::Multi { since: now, first: self.first_pos(pos), moved: true };
                    }
                    State::Scrolling { .. } => {
                        out.push(wheel(Vec2::ZERO, TouchPhase::End, modifiers));
                        self.state = State::Multi { since: now, first: self.first_pos(pos), moved: true };
                    }
                    State::Pending { .. } => {
                        self.state = State::Multi { since: now, first: self.first_pos(pos), moved: false };
                    }
                    State::Multi { .. } => {}
                }
            }
            TouchPhase::Move => {
                let prev = self.down.iter().find(|(d, _)| *d == id).map(|(_, p)| *p);
                if let Some(slot) = self.down.iter_mut().find(|(d, _)| *d == id) {
                    slot.1 = pos;
                }
                match self.state {
                    State::Pending { id: p, start, since, .. } if p == id => {
                        if (pos - start).length() > SLOP {
                            let delta = pos - start;
                            out.push(wheel(delta, TouchPhase::Start, modifiers));
                            let elapsed = ((now - since) as f32).max(1.0 / 120.0);
                            self.state = State::Scrolling { id, last: pos, last_time: now, velocity: delta / elapsed };
                        } else {
                            self.state = State::Pending { id, start, since, last: pos };
                        }
                    }
                    State::Scrolling { id: s, last, last_time, velocity } if s == id => {
                        let delta = pos - last;
                        out.push(wheel(delta, TouchPhase::Move, modifiers));
                        let dt = ((now - last_time) as f32).max(1.0 / 240.0);
                        // A smoothed velocity: the last few moves count most.
                        let velocity = if now > last_time { velocity * 0.3 + (delta / dt) * 0.7 } else { velocity };
                        self.state = State::Scrolling { id, last: pos, last_time: now, velocity };
                    }
                    State::Pressing { id: p } if p == id => out.push(Event::PointerMoved(pos)),
                    State::Multi { since, first, moved } => {
                        let wandered = prev.is_some_and(|p| (pos - p).length() > SLOP / 2.0);
                        self.state = State::Multi { since, first, moved: moved || wandered };
                    }
                    _ => {}
                }
            }
            TouchPhase::End | TouchPhase::Cancel => {
                self.down.retain(|(d, _)| *d != id);
                let cancelled = phase == TouchPhase::Cancel;
                match self.state {
                    State::Pending { id: p, start, .. } if p == id => {
                        if !cancelled {
                            // A tap: press now, release next frame, then the pointer goes away.
                            out.push(Event::PointerMoved(start));
                            out.push(button(start, PointerButton::Primary, true, modifiers));
                            self.next.push(button(start, PointerButton::Primary, false, modifiers));
                            self.next.push(Event::PointerGone);
                        } else {
                            out.push(Event::PointerGone);
                        }
                        self.state = State::Idle;
                    }
                    State::Scrolling { id: s, last_time, velocity, .. } if s == id => {
                        // A flick (moving at release) coasts; a swipe that stopped first doesn't.
                        let fresh = now - last_time < 0.08;
                        let speed = velocity.length();
                        if !cancelled && fresh && speed >= MIN_FLING {
                            let v = if speed > MAX_FLING { velocity * (MAX_FLING / speed) } else { velocity };
                            self.fling = Some((v, now));
                            out.push(wheel(Vec2::ZERO, TouchPhase::Move, modifiers));
                        } else {
                            out.push(wheel(Vec2::ZERO, TouchPhase::End, modifiers));
                        }
                        self.next.push(Event::PointerGone);
                        self.state = State::Idle;
                    }
                    State::Pressing { id: p } if p == id => {
                        out.push(button(pos, PointerButton::Primary, false, modifiers));
                        self.next.push(Event::PointerGone);
                        self.state = State::Idle;
                    }
                    State::Multi { since, first, moved } => {
                        if self.down.is_empty() {
                            if !cancelled && !moved && now - since <= TWO_FINGER_TAP {
                                out.push(Event::PointerMoved(first));
                                out.push(button(first, PointerButton::Secondary, true, modifiers));
                                self.next.push(button(first, PointerButton::Secondary, false, modifiers));
                            }
                            self.next.push(Event::PointerGone);
                            self.state = State::Idle;
                        }
                    }
                    _ => {
                        if self.down.is_empty() {
                            self.state = State::Idle;
                        }
                    }
                }
            }
        }
    }

    /// Where the first finger still down is (else `fallback`).
    fn first_pos(&self, fallback: Pos2) -> Pos2 {
        self.down.first().map(|(_, p)| *p).unwrap_or(fallback)
    }
}

fn wheel(delta: Vec2, phase: TouchPhase, modifiers: Modifiers) -> Event {
    Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase, source: MouseWheelSource::Trackpad, modifiers }
}

fn button(pos: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(id: u64, phase: TouchPhase, x: f32, y: f32) -> Event {
        Event::Touch { device_id: egui::TouchDeviceId(0), id: TouchId(id), phase, pos: egui::pos2(x, y), force: None }
    }

    /// What egui-winit adds for the first finger.
    fn emulated(phase: TouchPhase, x: f32, y: f32) -> Vec<Event> {
        let pos = egui::pos2(x, y);
        match phase {
            TouchPhase::Start => vec![Event::PointerMoved(pos), button(pos, PointerButton::Primary, true, Modifiers::NONE)],
            TouchPhase::Move => vec![Event::PointerMoved(pos)],
            _ => vec![button(pos, PointerButton::Primary, false, Modifiers::NONE), Event::PointerGone],
        }
    }

    fn frame(t: &mut Touches, time: f64, events: Vec<Event>) -> Vec<Event> {
        let mut raw = RawInput { time: Some(time), events, ..Default::default() };
        t.process(&mut raw);
        raw.events.into_iter().filter(|e| !matches!(e, Event::Touch { .. })).collect()
    }

    fn finger(id: u64, phase: TouchPhase, x: f32, y: f32) -> Vec<Event> {
        let mut v = vec![touch(id, phase, x, y)];
        if id == 0 {
            v.extend(emulated(phase, x, y));
        }
        v
    }

    fn presses(events: &[Event]) -> Vec<(PointerButton, bool)> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { button, pressed, .. } => Some((*button, *pressed)),
                _ => None,
            })
            .collect()
    }

    fn scrolled(events: &[Event]) -> Vec2 {
        events
            .iter()
            .map(|e| match e {
                Event::MouseWheel { delta, .. } => *delta,
                _ => Vec2::ZERO,
            })
            .fold(Vec2::ZERO, |a, b| a + b)
    }

    #[test]
    fn a_tap_is_a_click_split_over_two_frames() {
        let mut t = Touches::default();
        assert!(presses(&frame(&mut t, 0.0, finger(0, TouchPhase::Start, 50.0, 50.0))).is_empty(), "no press while it may still become a swipe");
        let up = frame(&mut t, 0.1, finger(0, TouchPhase::End, 51.0, 50.0));
        assert_eq!(presses(&up), vec![(PointerButton::Primary, true)]);
        let next = frame(&mut t, 0.11, vec![]);
        assert_eq!(presses(&next), vec![(PointerButton::Primary, false)]);
        assert!(next.iter().any(|e| matches!(e, Event::PointerGone)));
    }

    #[test]
    fn a_swipe_scrolls_and_never_presses() {
        let mut t = Touches::default();
        let mut all = frame(&mut t, 0.0, finger(0, TouchPhase::Start, 100.0, 300.0));
        all.extend(frame(&mut t, 0.05, finger(0, TouchPhase::Move, 100.0, 280.0)));
        all.extend(frame(&mut t, 0.10, finger(0, TouchPhase::Move, 100.0, 200.0)));
        all.extend(frame(&mut t, 0.50, finger(0, TouchPhase::End, 100.0, 200.0)));
        assert!(presses(&all).is_empty());
        assert_eq!(scrolled(&all), egui::vec2(0.0, -100.0), "content follows the finger up");
    }

    #[test]
    fn a_flick_coasts_and_stops() {
        let mut t = Touches::default();
        frame(&mut t, 0.0, finger(0, TouchPhase::Start, 100.0, 600.0));
        frame(&mut t, 0.016, finger(0, TouchPhase::Move, 100.0, 560.0));
        frame(&mut t, 0.032, finger(0, TouchPhase::Move, 100.0, 520.0));
        frame(&mut t, 0.048, finger(0, TouchPhase::End, 100.0, 520.0));
        let mut coast = Vec2::ZERO;
        let mut time = 0.048;
        for _ in 0..600 {
            time += 1.0 / 60.0;
            coast += scrolled(&frame(&mut t, time, vec![]));
            if t.fling.is_none() {
                break;
            }
        }
        assert!(t.fling.is_none(), "the coast ends");
        assert!(coast.y < -50.0, "it kept going up: {coast:?}");
    }

    #[test]
    fn a_long_press_holds_the_button_for_drag_select() {
        let mut t = Touches::default();
        frame(&mut t, 0.0, finger(0, TouchPhase::Start, 40.0, 40.0));
        assert!(presses(&frame(&mut t, 0.2, vec![])).is_empty());
        assert_eq!(presses(&frame(&mut t, 0.5, vec![])), vec![(PointerButton::Primary, true)]);
        let moved = frame(&mut t, 0.6, finger(0, TouchPhase::Move, 140.0, 40.0));
        assert!(moved.iter().any(|e| matches!(e, Event::PointerMoved(p) if p.x == 140.0)), "dragging moves the pointer");
        assert_eq!(scrolled(&moved), Vec2::ZERO);
        assert_eq!(presses(&frame(&mut t, 0.7, finger(0, TouchPhase::End, 140.0, 40.0))), vec![(PointerButton::Primary, false)]);
    }

    #[test]
    fn a_held_finger_drags_without_touches_so_the_grid_selects_instead_of_panning() {
        let mut t = Touches::default();
        frame(&mut t, 0.0, finger(0, TouchPhase::Start, 40.0, 40.0));
        frame(&mut t, 0.5, vec![]);
        let mut raw = RawInput { time: Some(0.6), events: finger(0, TouchPhase::Move, 140.0, 90.0), ..Default::default() };
        t.process(&mut raw);
        assert!(!raw.events.iter().any(|e| matches!(e, Event::Touch { .. })), "no touch while drag-selecting");
        let mut held = RawInput { time: Some(0.0), events: finger(1, TouchPhase::Start, 10.0, 10.0), ..Default::default() };
        let mut t2 = Touches::default();
        t2.process(&mut held);
        let mut due = RawInput { time: Some(0.5), ..Default::default() };
        t2.process(&mut due);
        assert!(due.events.iter().any(|e| matches!(e, Event::Touch { phase: TouchPhase::Cancel, .. })), "the held finger's touch is ended for egui");
        assert!(raw.events.iter().any(|e| matches!(e, Event::PointerMoved(p) if p.x == 140.0)));
    }

    #[test]
    fn a_two_finger_tap_is_a_right_click() {
        let mut t = Touches::default();
        let mut all = frame(&mut t, 0.0, finger(0, TouchPhase::Start, 40.0, 40.0));
        all.extend(frame(&mut t, 0.02, finger(1, TouchPhase::Start, 90.0, 40.0)));
        all.extend(frame(&mut t, 0.1, finger(1, TouchPhase::End, 90.0, 40.0)));
        all.extend(frame(&mut t, 0.12, finger(0, TouchPhase::End, 40.0, 40.0)));
        all.extend(frame(&mut t, 0.13, vec![]));
        assert_eq!(presses(&all), vec![(PointerButton::Secondary, true), (PointerButton::Secondary, false)]);
    }

    #[test]
    fn a_pinch_neither_clicks_nor_scrolls_but_keeps_the_touches() {
        let mut t = Touches::default();
        let mut raw = RawInput { time: Some(0.0), events: finger(0, TouchPhase::Start, 40.0, 40.0), ..Default::default() };
        t.process(&mut raw);
        let mut kept = raw.events.iter().filter(|e| matches!(e, Event::Touch { .. })).count();
        let mut all = Vec::new();
        for (time, events) in [
            (0.01, finger(1, TouchPhase::Start, 100.0, 40.0)),
            (0.1, finger(1, TouchPhase::Move, 160.0, 40.0)),
            (0.2, finger(1, TouchPhase::End, 160.0, 40.0)),
            (0.3, finger(0, TouchPhase::End, 40.0, 40.0)),
        ] {
            let mut raw = RawInput { time: Some(time), events, ..Default::default() };
            t.process(&mut raw);
            kept += raw.events.iter().filter(|e| matches!(e, Event::Touch { .. })).count();
            all.extend(raw.events);
        }
        assert_eq!(kept, 5, "egui still sees every touch, for pinch zoom");
        assert!(presses(&all).is_empty());
        assert_eq!(scrolled(&all), Vec2::ZERO);
    }

    #[test]
    fn keys_and_text_pass_through() {
        let mut t = Touches::default();
        let out = frame(&mut t, 0.0, vec![Event::Text("é".into())]);
        assert_eq!(out, vec![Event::Text("é".into())]);
    }
}
