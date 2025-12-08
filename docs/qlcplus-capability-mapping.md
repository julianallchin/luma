# QLC+ Capability Mapping Architecture

## Overview

QLC+ solves the "diverse fixture" problem through a capability-based channel lookup system. High-level functions (EFX, RGB Matrix) query fixtures by **what they can do** (capability) rather than **which DMX channels they use**.

---

## Core Architecture

### 1. Fixture Definition Layer (`resources/fixtures/*.qxf`)

XML files that define fixture capabilities:

```xml
<Channel Name="Pan" Preset="PositionPan">
  <Group Byte="0">Pan</Group>
</Channel>

<Channel Name="Red 1" Preset="IntensityRed">
  <Group Byte="0">Intensity</Group>
  <Colour>Red</Colour>
</Channel>

<Mode Name="Extended">
  <Channel Number="0">Pan</Channel>
  <Channel Number="13">Red 1</Channel>
  <Head>
    <Channel>13</Channel>  <!-- Red 1 -->
    <Channel>14</Channel>  <!-- Green 1 -->
    <Channel>15</Channel>  <!-- Blue 1 -->
  </Head>
</Mode>
```

**Key metadata:**
- `Group`: High-level capability category (Pan, Tilt, Intensity, Colour, etc.)
- `Preset`: Specific semantic meaning (PositionPan, IntensityRed, etc.)
- `<Head>`: Groups channels into primitives for multi-head fixtures

---

### 2. Channel Type System (`engine/src/qlcchannel.h`)

**Channel Groups** (broad categories):
```cpp
enum Group {
    Intensity = 0,
    Colour,
    Gobo,
    Speed,
    Pan,
    Tilt,
    Shutter,
    Prism,
    Beam,
    Effect,
    Maintenance,
    Nothing,
    NoGroup = INT_MAX
};
```

**Channel Presets** (specific semantics):
```cpp
enum Preset {
    Custom = 0,
    IntensityMasterDimmer,
    IntensityDimmer,
    IntensityRed,
    IntensityGreen,
    IntensityBlue,
    IntensityWhite,
    IntensityAmber,
    IntensityUV,
    PositionPan,
    PositionPanFine,
    PositionTilt,
    PositionTiltFine,
    ColorMacro,
    ColorWheel,
    GoboWheel,
    ShutterStrobeSlowFast,
    BeamZoomSmallBig,
    // ... ~50+ total presets
};
```

---

### 3. Fixture Head Structure (`engine/src/qlcfixturehead.h/cpp`)

Each "head" (primitive) maintains a **capability map**:

```cpp
class QLCFixtureHead {
private:
    // Maps: Channel Group → DMX channel number(s)
    // Upper 16 bits = MSB channel, Lower 16 bits = LSB channel
    QHash<int, quint32> m_channelsMap;

public:
    // Returns DMX channel number for a capability, or invalid if N/A
    quint32 channelNumber(int type, int controlByte) const {
        quint32 val = m_channelsMap.value(type, 0xFFFFFFFF);

        if (val == 0xFFFFFFFF)
            return QLCChannel::invalid();

        if (controlByte == QLCChannel::MSB)
            val = val >> 16;  // Get MSB channel
        else
            val &= 0x0000FFFF;  // Get LSB channel

        if (val == 0x0000FFFF)
            return QLCChannel::invalid();

        return val;
    }

    // Convenience methods
    QVector<quint32> rgbChannels() const;
    QVector<quint32> cmyChannels() const;
};
```

**The map is built from fixture definitions:**
- `Pan` → channel 13
- `Tilt` → channel 15
- `Intensity` → channel 0
- etc.

---

### 4. Fixture Runtime Interface (`engine/src/fixture.h/cpp`)

Fixtures expose capability queries:

```cpp
class Fixture {
public:
    // Get channel number for a capability on a specific head
    // type: QLCChannel::Group enum value
    // controlByte: MSB or LSB (for 16-bit channels)
    // head: Which primitive (for multi-head fixtures)
    quint32 channelNumber(int type, int controlByte, int head = 0) const {
        if (m_fixtureMode == NULL || head < 0 || head >= heads().size())
            return QLCChannel::invalid();

        return m_fixtureMode->heads().at(head).channelNumber(type, controlByte);
    }

    // Get global master dimmer (if exists)
    quint32 masterIntensityChannel() const;

    // Get RGB channels for a head
    QVector<quint32> rgbChannels(int head = 0) const;

    // Get CMY channels for a head
    QVector<quint32> cmyChannels(int head = 0) const;
};
```

---

### 5. Usage Example: EFX Engine (`engine/src/efx.cpp`, `efxfixture.cpp`)

The EFX engine applies position patterns (circles, squares, etc.) to fixtures with pan/tilt:

```cpp
// efxfixture.cpp - EFXFixture::setMode()

// Check if this fixture has pan/tilt capability
if (fxi->channelNumber(QLCChannel::Pan, QLCChannel::MSB, head().head) != QLCChannel::invalid() ||
    fxi->channelNumber(QLCChannel::Tilt, QLCChannel::MSB, head().head) != QLCChannel::invalid())
{
    // YES - this fixture can move

    // Look up the actual DMX channels
    m_firstMsbChannel = fxi->channelNumber(QLCChannel::Pan, QLCChannel::MSB, head().head);
    m_firstLsbChannel = fxi->channelNumber(QLCChannel::Pan, QLCChannel::LSB, head().head);
    m_secondMsbChannel = fxi->channelNumber(QLCChannel::Tilt, QLCChannel::MSB, head().head);
    m_secondLsbChannel = fxi->channelNumber(QLCChannel::Tilt, QLCChannel::LSB, head().head);

    // Now generate pattern and write to those channels
}
else
{
    // NO - fixture is fixed, skip position control
}
```

**Key insight:** The EFX doesn't hardcode "pan is channel 0, tilt is channel 2" - it **asks the fixture** where its pan/tilt channels are.

---

### 6. Usage Example: RGB Matrix (`engine/src/rgbmatrix.cpp`)

RGB Matrix generates pixel patterns and needs to find color channels:

```cpp
// Get RGB channels from fixture head
QVector<quint32> rgb = fxi->rgbChannels(head);

if (rgb.isEmpty())
{
    // No RGB mixing capability - skip this head
}
else
{
    // rgb[0] = red channel, rgb[1] = green, rgb[2] = blue
    // Write computed colors to those channels
    dmxBuffer[rgb[0]] = computedColor.red();
    dmxBuffer[rgb[1]] = computedColor.green();
    dmxBuffer[rgb[2]] = computedColor.blue();
}
```

**Handles diverse color mixing:**
- RGB fixtures: Returns [R, G, B]
- RGBW fixtures: Returns [R, G, B, W] (matrix ignores W or uses brightness)
- RGBA fixtures: Returns [R, G, B, A]
- CMY fixtures: Different method (`cmyChannels()`)

---

## Important Files Reference

### Fixture Definitions
- **Path**: `resources/fixtures/[Manufacturer]/[Model].qxf`
- **Format**: XML with channel definitions, groups, presets, modes, heads
- **Example**: `resources/fixtures/Venue/Venue-Tetra-Bar.qxf`

### Channel Type System
- **`engine/src/qlcchannel.h`**: Defines `Group` enum, `Preset` enum
- **`engine/src/qlcchannel.cpp`**: String conversions, icon mapping

### Head/Mode Structure
- **`engine/src/qlcfixturehead.h/cpp`**: Manages per-head capability maps
- **`engine/src/qlcfixturemode.h/cpp`**: Manages modes (channel layouts)
- **Key method**: `QLCFixtureHead::channelNumber(type, controlByte)`

### Fixture Runtime
- **`engine/src/fixture.h/cpp`**: Runtime fixture instance
- **Key methods**:
  - `channelNumber(type, controlByte, head)`
  - `rgbChannels(head)`
  - `masterIntensityChannel()`

### High-Level Functions Using Capabilities
- **`engine/src/efx.cpp`**: Position effects (pan/tilt patterns)
- **`engine/src/efxfixture.cpp`**: Per-fixture EFX logic, channel lookup
- **`engine/src/rgbmatrix.cpp`**: Pixel/color effects (RGB/intensity patterns)
- **`engine/src/genericfader.cpp`**: Channel fading with HTP/LTP rules

---

## What QLC+ Does

✅ **Capability-based channel lookup**
✅ **Heterogeneous fixtures in effects** (EFX works on any moving head)
✅ **Graceful degradation** (missing capabilities are skipped)
✅ **Multi-head fixture support** (via `<Head>` grouping)
✅ **16-bit channel support** (MSB/LSB for pan/tilt/dimmer)
✅ **Color space abstraction** (RGB, RGBW, CMY handled transparently)

---

## What QLC+ Does NOT Do (That Luma Needs)

❌ **Semantic venue grouping** (no "The Ceiling" concept)
❌ **Portable patterns** (effects are tied to specific fixture instances)
❌ **Automatic capability routing** (user manually adds fixtures to effects)
❌ **Compositing/layering system** (no blend modes, z-index, or multi-pattern blending)
❌ **Normalized capability buffer** (works directly in DMX space, not abstract color/position)

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────┐
│ Fixture Definition (XML)                        │
│  - Channels with Groups/Presets                 │
│  - Modes (channel→number mapping)               │
│  - Heads (primitive groupings)                  │
└─────────────┬───────────────────────────────────┘
              │ loaded by
              ▼
┌─────────────────────────────────────────────────┐
│ QLCFixtureMode                                  │
│  - Creates QLCFixtureHead[] for each primitive  │
│  - Builds capability maps                       │
└─────────────┬───────────────────────────────────┘
              │ used by
              ▼
┌─────────────────────────────────────────────────┐
│ Fixture (runtime instance)                      │
│  Methods:                                       │
│   - channelNumber(Group, MSB/LSB, head) → ch#   │
│   - rgbChannels(head) → [R, G, B]              │
│   - masterIntensityChannel() → ch#              │
└─────────────┬───────────────────────────────────┘
              │ queried by
              ▼
┌─────────────────────────────────────────────────┐
│ High-Level Functions                            │
│  - EFX: Queries Pan/Tilt, generates patterns   │
│  - RGBMatrix: Queries RGB, generates colors    │
│  - Writes directly to DMX buffer                │
└─────────────────────────────────────────────────┘
```

---

## Key Lesson for Luma

QLC+'s `channelNumber(capability, head)` system is **exactly what you need** at the DMX rendering stage.

**Your Apply node should work like:**
```typescript
Apply(capability="color", data: Color[]) {
  for (primitive of selectedPrimitives) {
    const rgbChannels = primitive.fixture.rgbChannels(primitive.head);
    if (rgbChannels.length >= 3) {
      dmxBuffer[rgbChannels[0]] = data.r;
      dmxBuffer[rgbChannels[1]] = data.g;
      dmxBuffer[rgbChannels[2]] = data.b;
    }
  }
}
```

The innovation is:
1. **Before this**: Semantic selection + compositing in normalized space
2. **At this step**: Capability→DMX channel lookup (just like QLC+)
