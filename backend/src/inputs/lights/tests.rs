//! Frames of lights: what changed is sent, and nothing else.

use super::*;
use crate::controls::{Job, Look};
use crate::inputs::{Mode, Profile};

fn controller(id: &str, port: &str) -> Controller {
    Controller {
        id: id.into(),
        name: id.into(),
        port: port.into(),
        profile: Profile::default(),
    }
}

fn binding(controller: &str, control: &str, number: u8) -> Binding {
    Binding {
        id: format!("{controller}-{control}"),
        controller_id: controller.into(),
        control: control.into(),
        address: Address::MidiNote { channel: 0, number },
        mode: Mode::Momentary,
        invert: false,
    }
}

fn look(on: bool) -> Look {
    Look {
        job: Job::Effect,
        on,
        waiting: false,
        held: false,
        selected: false,
        color: [1.; 3],
        value: 0.,
        beat: None,
    }
}

fn surface(looks: &[(&str, Look)]) -> Surface {
    looks
        .iter()
        .map(|(name, look)| ((*name).to_owned(), look.clone()))
        .collect()
}

#[test]
fn a_frame_sends_only_the_controls_whose_look_changed() {
    let controllers = [controller("pads", "Pads"), controller("away", "Unplugged")];
    let bindings = [
        binding("pads", "a", 1),
        binding("pads", "b", 2),
        binding("away", "a", 1),
    ];
    let ports = ["Pads".to_owned()];
    let mut lights = Lights::default();
    let first = surface(&[("a", look(true)), ("b", look(false))]);
    let sent = lights.frame(&controllers, &bindings, &first, &ports, true);
    assert_eq!(
        sent,
        vec![
            ("Pads".to_owned(), vec![0x90, 1, 127]),
            ("Pads".to_owned(), vec![0x90, 2, 0]),
        ],
        "every bound control once, and none on a port that is not open"
    );
    assert!(
        lights
            .frame(&controllers, &bindings, &first, &ports, true)
            .is_empty(),
        "nothing changed, nothing sent"
    );
    let second = surface(&[("a", look(true)), ("b", look(true))]);
    assert_eq!(
        lights.frame(&controllers, &bindings, &second, &ports, true),
        vec![("Pads".to_owned(), vec![0x90, 2, 127])]
    );

    // A control that leaves the surface goes dark, once.
    let third = surface(&[("b", look(true))]);
    assert_eq!(
        lights.frame(&controllers, &bindings, &third, &ports, true),
        vec![("Pads".to_owned(), vec![0x90, 1, 0])]
    );
    assert!(lights
        .frame(&controllers, &bindings, &third, &ports, true)
        .is_empty());

    // A port that comes back is drawn whole.
    lights.frame(&controllers, &bindings, &third, &[], true);
    assert_eq!(
        lights.frame(&controllers, &bindings, &third, &ports, true),
        vec![("Pads".to_owned(), vec![0x90, 2, 127])]
    );
}

#[test]
fn a_blink_drawn_here_turns_on_and_off_with_the_phase() {
    let controllers = [controller("pads", "Pads")];
    let bindings = [binding("pads", "a", 1)];
    let ports = ["Pads".to_owned()];
    let waiting = surface(&[(
        "a",
        Look {
            waiting: true,
            ..look(false)
        },
    )]);
    let mut lights = Lights::default();
    let lit = lights.frame(&controllers, &bindings, &waiting, &ports, true);
    let dark = lights.frame(&controllers, &bindings, &waiting, &ports, false);
    assert_eq!(lit, vec![("Pads".to_owned(), vec![0x90, 1, 127])]);
    assert_eq!(dark, vec![("Pads".to_owned(), vec![0x90, 1, 0])]);
}

#[test]
fn a_controller_forgotten_is_drawn_whole_again() {
    let controllers = [controller("pads", "Pads"), controller("keys", "Keys")];
    let bindings = [binding("pads", "a", 1), binding("keys", "a", 1)];
    let ports = ["Pads".to_owned(), "Keys".to_owned()];
    let mut lights = Lights::default();
    let shown = surface(&[("a", look(true))]);
    assert_eq!(
        lights
            .frame(&controllers, &bindings, &shown, &ports, true)
            .len(),
        2
    );
    lights.forget(&["pads".to_owned()]);
    assert_eq!(
        lights.frame(&controllers, &bindings, &shown, &ports, true),
        vec![("Pads".to_owned(), vec![0x90, 1, 127])],
        "its lights again, after a light set by hand or cleared; the other's not"
    );
}
