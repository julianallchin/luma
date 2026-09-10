#![cfg(feature = "app")]
use super::support;
use gpui_agent::Mode;
use std::time::Duration;

#[test]
fn returning_to_a_parked_song_plays_its_own_audio() {
    let mut harness = support::Fixture::new("track-playback-sessions", 20, vec![])
        .with_second_track(8)
        .with_seeded_threads()
        .open(Mode::Headless);
    let result = harness.exec(&support::script(r#"
        function node(role,label) { return app.snapshot().find({role,label}); }
        function time() { return app.snapshot().findAll({role:"text"})
            .find(n => /^\d+:\d+ \/ \d+:\d+$/.test(n.label))?.label; }
        function play(duration) {
            nav.step("ready player", "button", "Play");
            until("playing the selected song", s => !!s.find({role:"button",label:"Pause"}));
            app.frames(4,{waitMs:60});
            const reading = time();
            if (!reading?.endsWith(" / " + duration)) throw new Error("wrong audio duration: " + reading);
            return reading;
        }
        nav.venue("Test Venue");
        nav.step("all tracks", "toggle", "All");
        nav.step("tracks in all venues", "toggle", "In Venue");
        nav.track("Aurora");
        until("Aurora waveform", () => node("card","Waveform"));
        const first = play("0:20");
        nav.track("Zulu");
        until("Zulu waveform", () => node("card","Waveform"));
        const second = play("0:08");
        nav.track("Aurora");
        const returned = play("0:20");
        // Repeated switches exercise old poll loops as well as the cached tab.
        nav.track("Zulu");
        const again = play("0:08");
        nav.track("Aurora");
        const final = play("0:20");
        nav.step("pause", "button", "Pause");
        until("stopped", s => !!s.find({role:"button",label:"Play"}));
        ({first,second,returned,again,final})
    "#), Duration::from_secs(120));
    assert_eq!(result.error, None, "{}\n{:#}", result.stdout, result.result);
}
