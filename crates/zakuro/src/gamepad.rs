//! game controllers, any the system knows, read through gilrs.

use gilrs::{Axis, Button, EventType, Gilrs};
use zakuro_core::services::hid::{InputState, PadState};

use crate::settings::PadButtons;

/// how far a stick has to lean before it counts, some rest a little off
/// center.
const DEADZONE: f32 = 0.15;

pub struct Gamepads {
    /// none when the system could not be asked for controllers.
    gilrs: Option<Gilrs>,
    buttons: PadButtons,
    /// the right stick moves the pointer instead of being the C-stick.
    pointer: bool,
}

impl Gamepads {
    pub fn new(buttons: PadButtons) -> Gamepads {
        let gilrs = Gilrs::new()
            .inspect_err(|error| log::warn!("controllers are not available, {error}"))
            .ok();
        Gamepads { gilrs, buttons, pointer: false }
    }

    pub fn set_buttons(&mut self, buttons: PadButtons) {
        self.buttons = buttons;
    }

    pub fn set_pointer(&mut self, pointer: bool) {
        self.pointer = pointer;
    }

    /// how far the right stick leans, right and down positive, when it
    /// moves the pointer.
    pub fn pointer_motion(&self) -> (f32, f32) {
        let Some(gilrs) = self.gilrs.as_ref().filter(|_| self.pointer) else { return (0.0, 0.0) };
        for (_, pad) in gilrs.gamepads() {
            let (x, y) = (pad.value(Axis::RightStickX), pad.value(Axis::RightStickY));
            if !self.pointer && x.hypot(y) > DEADZONE {
                return (x.clamp(-1.0, 1.0), -y.clamp(-1.0, 1.0));
            }
        }
        (0.0, 0.0)
    }

    /// whether the right stick is clicked to touch where the pointer is.
    pub fn pointer_touching(&self) -> bool {
        self.pointer && self.holding(Button::RightThumb)
    }

    /// takes in what the controllers did since the last call, and returns the
    /// buttons pressed meanwhile.
    pub fn poll(&mut self) -> Vec<Button> {
        let mut pressed = Vec::new();
        let Some(gilrs) = &mut self.gilrs else { return pressed };
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::ButtonPressed(button, _) => pressed.push(button),
                EventType::Connected => {
                    log::info!("controller connected, {}", gilrs.gamepad(event.id).name());
                }
                EventType::Disconnected => log::info!("controller disconnected"),
                _ => {}
            }
        }
        pressed
    }

    /// whether a controller holds button down.
    pub fn holding(&self, button: Button) -> bool {
        self.gilrs.as_ref().is_some_and(|gilrs| gilrs.gamepads().any(|(_, pad)| pad.is_pressed(button)))
    }

    /// what the controllers hold down, over what the keyboard does.
    pub fn apply(&self, mut state: InputState) -> InputState {
        let Some(gilrs) = &self.gilrs else { return state };
        for (_, pad) in gilrs.gamepads() {
            for (button, bound) in self.buttons.map() {
                if pad.is_pressed(bound) {
                    state.buttons.set(button, true);
                }
            }
            // the d-pad of some controllers is a pair of axes
            let dpad = [
                (PadState::RIGHT, pad.value(Axis::DPadX) > 0.5),
                (PadState::LEFT, pad.value(Axis::DPadX) < -0.5),
                (PadState::UP, pad.value(Axis::DPadY) > 0.5),
                (PadState::DOWN, pad.value(Axis::DPadY) < -0.5),
            ];
            for (button, held) in dpad {
                if held {
                    state.buttons.set(button, true);
                }
            }
            // the left stick is the circle pad and the right one the
            // C-stick, up positive as the console has it
            let (x, y) = (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY));
            if x.hypot(y) > DEADZONE {
                state.circle_x = x.clamp(-1.0, 1.0);
                state.circle_y = y.clamp(-1.0, 1.0);
            }
            let (x, y) = (pad.value(Axis::RightStickX), pad.value(Axis::RightStickY));
            if x.hypot(y) > DEADZONE {
                state.c_stick_x = x.clamp(-1.0, 1.0);
                state.c_stick_y = y.clamp(-1.0, 1.0);
            }
        }
        state
    }
}

/// a controller button as a person would call it.
pub fn button_name(button: Button) -> &'static str {
    match button {
        Button::South => "Bottom face",
        Button::East => "Right face",
        Button::North => "Top face",
        Button::West => "Left face",
        Button::LeftTrigger => "LB / L1",
        Button::RightTrigger => "RB / R1",
        Button::LeftTrigger2 => "LT / L2",
        Button::RightTrigger2 => "RT / R2",
        Button::Select => "Back / Select",
        Button::Start => "Start",
        Button::Mode => "Home",
        Button::LeftThumb => "Left stick",
        Button::RightThumb => "Right stick",
        Button::DPadUp => "D-pad up",
        Button::DPadDown => "D-pad down",
        Button::DPadLeft => "D-pad left",
        Button::DPadRight => "D-pad right",
        Button::C => "C",
        Button::Z => "Z",
        Button::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// with no controller, or no way to ask for one, the keyboard's input
    /// goes through as it was.
    #[test]
    fn no_controller_leaves_the_keyboard_alone() {
        let gamepads = Gamepads::new(PadButtons::default());
        if gamepads.gilrs.as_ref().is_some_and(|gilrs| gilrs.gamepads().next().is_some()) {
            return;
        }
        let state = InputState { buttons: PadState::A, circle_x: 1.0, circle_y: 0.0, touch: Some((1, 2)), ..InputState::default() };
        let applied = gamepads.apply(state);
        assert_eq!(applied.buttons, PadState::A);
        assert_eq!((applied.circle_x, applied.circle_y, applied.touch), (1.0, 0.0, Some((1, 2))));
    }
}
