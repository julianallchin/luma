# Laser hardware

What stands between the code and a projector drawing the right picture.
[lasers.md](lasers.md) is the design; this is the bench. Everything here has
run only against an emulator written from the Ether Dream protocol page
(`backend/src/output/ether_dream.rs`, `mod tests`), never a DAC.

## The path

```
clip graph → UniverseState → LaserRig::solve → LaserFrame → WireFrame → LaserDriver → DAC
              eval/scene.rs    laser/mod.rs      laser/scan.rs  output/laser.rs  output/ether_dream.rs
```

- `LaserFrame`: per projector, points of galvo position (−1 to 1) and each
  diode's drive level (0 to 1), in scan order, at the scanner's rate.
- `WireFrame::of(frame, settings, master)`: the same points as the box is
  wired: mirrored, swapped, sized, offset, colour delayed, lifted to each
  diode's threshold, scaled by the master. 16-bit.
- `LaserDriver`: finds devices and plays `laser::Shared`'s newest frame for
  each projector routed to it. The hub (`output/mod.rs`) owns the tick.

Rig → Lasers is the whole operator surface: the output switch, the window,
what the venue's projectors are, and each projector's DAC, test pattern,
output trim and limit. The test pattern and every edit show on the stage
too, so most of this can be tried with no DAC.

## First light

1. Projector's own key off, shutter closed or a beam block at the aperture.
   Set a limit and a low power cap before anything else.
2. Add a Laser Projector, open Rig → Lasers. The DAC should be listed under
   **DAC** within a second of joining the network (UDP broadcast, port 7654).
   If not: `listen` and `heard` in `ether_dream.rs`, and the firewall.
3. Bind it, turn on **Laser output**, press **Test**. The row should read
   "Playing · n scans a second". A fault reads as the DAC's own words.
4. Open the shutter. The test pattern is the window's edge and a cross at
   its middle, at a quarter of the capped power.

## To check, in order

Each is an assumption the emulator cannot test.

- **Byte order and layout** of the status and point packets (`Status::read`,
  `heard`, `Link::write`). Little-endian by the protocol page.
- **Prepare, begin, e-stop.** A DAC that powers up in e-stop is cleared,
  prepared, then begun at the scanner's rate (`Link::play`). Watch for an
  answer that is not an ACK, and for the playback state after an underflow.
- **Which way is up and across.** Mirror and Swap under Setup put it right;
  if every box needs the same ones, flip the default in `WireFrame::of`.
- **Which mirror is first** (`Projector::galvo`): the code takes the first
  to turn the beam across and the second to tip it. Draw a level line off
  centre: it should bow as the picture in the visualizer does. If it bows
  the other way, swap the two in `galvo` and `direction`.
- **Scan angle.** Full deflection is taken to be the projector's scan angle.
  A square of known size on a wall at a known distance says whether it is.
- **Scan plan** (`laser/scan.rs`): `LINE_STEP`, `BLANK_STEP`, `SETTLE`,
  `DWELL`. Tails at the end of a blanked jump mean more settle; a dim start
  to a line means more colour delay (a setting, in points).
- **Latency.** The driver keeps about 40 ms buffered (`BUFFER_S`), so lasers
  run that far behind the lights. Nothing compensates yet; the place to do
  it is where the hub solves, by solving that far ahead.
- **TTL projectors.** The power cap and the master scale an analog level. A
  TTL diode is on or off, so neither dims it; only the limit and blanking
  hold.

## Another DAC

Implement `output::laser::LaserDriver`, add its kind to
`models::lasers::LaserDeviceKind`, and start it beside `EtherDream::start`
in `output/mod.rs`. The emulator in `ether_dream.rs`'s tests is the
pattern for testing one without hardware.
