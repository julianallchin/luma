# Luma Compositing Buffer Design

## Overview

The compositing buffer is the **internal representation** of what each primitive should be doing **before** conversion to DMX. It exists in normalized, capability-based space and supports multi-layer blending.

---

## Architecture Flow

```
┌──────────────┐
│ Pattern 1    │ z-index: 1, blend: "add"
│ (Background) │
└──────┬───────┘
       │ outputs
       ▼
┌─────────────────────────────┐         ┌──────────────────┐
│ Primitive States (Layer 1)  │         │ Compositing      │
│  - Primitive A: {color, ...}│         │ Engine           │
│  - Primitive B: {color, ...}│────────▶│  - Merge layers  │
└─────────────────────────────┘         │  - Apply blends  │
                                        │  - Resolve caps  │
┌──────────────┐                        └────────┬─────────┘
│ Pattern 2    │ z-index: 2, blend: "multiply"          │
│ (Rhythm)     │                                        │
└──────┬───────┘                                        ▼
       │ outputs                        ┌──────────────────────────┐
       ▼                                │ Final Composite Buffer   │
┌─────────────────────────────┐        │  - Per-primitive state   │
│ Primitive States (Layer 2)  │        │  - Normalized values     │
│  - Primitive A: {intensity} │────────│  - Capability-based      │
│  - Primitive B: {intensity} │        └────────┬─────────────────┘
└─────────────────────────────┘                 │
                                                ▼
┌──────────────┐                        ┌──────────────────────────┐
│ Pattern 3    │ z-index: 3, blend: "replace"  │ DMX Renderer         │
│ (Impact)     │                        │  - Lookup capabilities   │
└──────┬───────┘                        │  - Map to DMX channels   │
       │ outputs                        │  - Convert to 0-255      │
       ▼                                └──────────────────────────┘
┌─────────────────────────────┐
│ Primitive States (Layer 3)  │
│  - Primitive C: {strobe}    │────────┐
└─────────────────────────────┘        │
                                       ▼
                              ┌──────────────────┐
                              │ DMX Universe[]   │
                              │  - Raw bytes     │
                              └──────────────────┘
```

---

## Data Structures

### 1. Primitive State (Per-Layer Output)

Each pattern outputs a collection of primitive states in **normalized, capability-based space**:

```typescript
interface Color {
  r: number;  // 0-1
  g: number;  // 0-1
  b: number;  // 0-1
  w?: number; // 0-1 (optional, for RGBW fixtures)
}

interface Position {
  pan?: number;   // -1 to 1 (normalized, 0 = center)
  tilt?: number;  // -1 to 1 (normalized, 0 = center)
}

interface Strobe {
  enabled: boolean;
  rate: number;  // 0-1 (normalized, maps to Hz range)
}

interface PrimitiveState {
  primitiveId: string;  // e.g., "fixture-1-head-0"

  // Capabilities (all optional - sparse representation)
  color?: Color;
  intensity?: number;     // 0-1
  position?: Position;
  strobe?: Strobe;
  zoom?: number;          // 0-1
  focus?: number;         // 0-1
  // ... extensible for more capabilities
}

interface LayerOutput {
  zIndex: number;
  blendMode: BlendMode;
  primitives: PrimitiveState[];
}
```

**Key points:**
- **Normalized values** (0-1, not DMX 0-255)
- **Sparse representation** (only include capabilities being controlled)
- **Primitive-granular** (not fixture-level - each head/pixel is separate)
- **Capability-typed** (color, position, intensity are distinct)

---

### 2. Composite Buffer (Final Merged State)

After compositing all layers, the buffer contains the **final state** for each primitive:

```typescript
interface CompositeBuffer {
  // Map: primitive ID → final merged state
  primitives: Map<string, PrimitiveState>;

  // Metadata
  timestamp: number;  // When this frame was computed
}
```

**This is the handoff point to DMX rendering.**

---

## Blend Modes

Each layer specifies how it combines with layers below it:

```typescript
enum BlendMode {
  // Color/Intensity blending
  Replace = "replace",    // Top layer overwrites bottom (default for most)
  Add = "add",           // Add values (clamped to 1.0)
  Multiply = "multiply", // Multiply values (darkening)
  Screen = "screen",     // Inverse multiply (lightening)
  Overlay = "overlay",   // Combination of multiply/screen

  // Special modes
  Max = "max",           // Take maximum value
  Min = "min",           // Take minimum value

  // Position blending
  PositionAdd = "position_add",  // Offset position (pan/tilt accumulate)
}
```

### Blend Mode Application (Per-Capability)

Blending happens **per capability**, not globally:

```typescript
function blendCapability(
  bottom: number | Color | Position | undefined,
  top: number | Color | Position | undefined,
  mode: BlendMode
): number | Color | Position | undefined {

  if (bottom === undefined) return top;
  if (top === undefined) return bottom;

  // For scalar values (intensity, zoom, etc.)
  if (typeof bottom === 'number' && typeof top === 'number') {
    switch (mode) {
      case BlendMode.Replace:
        return top;
      case BlendMode.Add:
        return Math.min(1.0, bottom + top);
      case BlendMode.Multiply:
        return bottom * top;
      case BlendMode.Screen:
        return 1.0 - (1.0 - bottom) * (1.0 - top);
      case BlendMode.Max:
        return Math.max(bottom, top);
      case BlendMode.Min:
        return Math.min(bottom, top);
    }
  }

  // For colors
  if (isColor(bottom) && isColor(top)) {
    return {
      r: blendChannel(bottom.r, top.r, mode),
      g: blendChannel(bottom.g, top.g, mode),
      b: blendChannel(bottom.b, top.b, mode),
      w: blendChannel(bottom.w, top.w, mode),
    };
  }

  // For positions (special handling)
  if (isPosition(bottom) && isPosition(top)) {
    if (mode === BlendMode.PositionAdd) {
      return {
        pan: (bottom.pan ?? 0) + (top.pan ?? 0),
        tilt: (bottom.tilt ?? 0) + (top.tilt ?? 0),
      };
    } else {
      // Default: replace
      return top;
    }
  }
}
```

---

## Compositing Algorithm

### Step 1: Sort Layers by Z-Index

```typescript
const sortedLayers = layers.sort((a, b) => a.zIndex - b.zIndex);
```

### Step 2: Merge Primitive States

```typescript
function compositeLayers(layers: LayerOutput[]): CompositeBuffer {
  const buffer = new Map<string, PrimitiveState>();

  // Process layers bottom-to-top (ascending z-index)
  for (const layer of sortedLayers) {
    for (const primitiveState of layer.primitives) {
      const existing = buffer.get(primitiveState.primitiveId);

      if (!existing) {
        // First layer to touch this primitive
        buffer.set(primitiveState.primitiveId, primitiveState);
      } else {
        // Merge with existing state
        const merged = mergePrimitiveStates(
          existing,
          primitiveState,
          layer.blendMode
        );
        buffer.set(primitiveState.primitiveId, merged);
      }
    }
  }

  return { primitives: buffer, timestamp: Date.now() };
}
```

### Step 3: Merge Individual Primitive States

```typescript
function mergePrimitiveStates(
  bottom: PrimitiveState,
  top: PrimitiveState,
  blendMode: BlendMode
): PrimitiveState {
  return {
    primitiveId: bottom.primitiveId,

    // Blend each capability independently
    color: blendCapability(bottom.color, top.color, blendMode),
    intensity: blendCapability(bottom.intensity, top.intensity, blendMode),
    position: blendCapability(bottom.position, top.position, blendMode),
    strobe: top.strobe ?? bottom.strobe,  // Strobe is replace-only
    zoom: blendCapability(bottom.zoom, top.zoom, blendMode),
    focus: blendCapability(bottom.focus, top.focus, blendMode),
  };
}
```

---

## Example: Three-Layer Composite

**Setup:**
```typescript
// Layer 1 (Background): Blue wash, z-index 1, blend "replace"
const layer1: LayerOutput = {
  zIndex: 1,
  blendMode: BlendMode.Replace,
  primitives: [
    { primitiveId: "fixture-1-head-0", color: { r: 0, g: 0, b: 1 }, intensity: 0.5 },
    { primitiveId: "fixture-2-head-0", color: { r: 0, g: 0, b: 1 }, intensity: 0.5 },
  ]
};

// Layer 2 (Rhythm): Intensity pulse, z-index 2, blend "multiply"
const layer2: LayerOutput = {
  zIndex: 2,
  blendMode: BlendMode.Multiply,
  primitives: [
    { primitiveId: "fixture-1-head-0", intensity: 0.8 },  // Only controls intensity
    { primitiveId: "fixture-2-head-0", intensity: 0.3 },
  ]
};

// Layer 3 (Impact): Red flash on fixture 1, z-index 3, blend "add"
const layer3: LayerOutput = {
  zIndex: 3,
  blendMode: BlendMode.Add,
  primitives: [
    { primitiveId: "fixture-1-head-0", color: { r: 1, g: 0, b: 0 } },  // Red added to blue
  ]
};
```

**Compositing Result:**
```typescript
CompositeBuffer {
  primitives: Map {
    "fixture-1-head-0" => {
      primitiveId: "fixture-1-head-0",
      color: { r: 1.0, g: 0, b: 1.0 },  // Blue + Red = Magenta (clamped)
      intensity: 0.4,  // 0.5 * 0.8 = 0.4 (multiply)
    },
    "fixture-2-head-0" => {
      primitiveId: "fixture-2-head-0",
      color: { r: 0, g: 0, b: 1.0 },  // Blue (no layer 3 on this primitive)
      intensity: 0.15,  // 0.5 * 0.3 = 0.15 (multiply)
    }
  }
}
```

---

## DMX Rendering (Final Step)

The composite buffer is converted to DMX using capability lookup:

```typescript
function renderToDMX(
  buffer: CompositeBuffer,
  fixtures: Fixture[]
): Uint8Array {
  const dmx = new Uint8Array(512);  // One universe

  for (const [primitiveId, state] of buffer.primitives) {
    const primitive = findPrimitive(fixtures, primitiveId);

    // Render color capability
    if (state.color) {
      const rgbChannels = primitive.fixture.rgbChannels(primitive.head);
      if (rgbChannels.length >= 3) {
        dmx[rgbChannels[0]] = Math.round(state.color.r * 255);
        dmx[rgbChannels[1]] = Math.round(state.color.g * 255);
        dmx[rgbChannels[2]] = Math.round(state.color.b * 255);
        if (rgbChannels.length >= 4 && state.color.w !== undefined) {
          dmx[rgbChannels[3]] = Math.round(state.color.w * 255);
        }
      }
    }

    // Render intensity capability
    if (state.intensity !== undefined) {
      const dimmerCh = primitive.fixture.channelNumber(
        ChannelGroup.Intensity,
        ChannelByte.MSB,
        primitive.head
      );
      if (dimmerCh !== null) {
        dmx[dimmerCh] = Math.round(state.intensity * 255);
      }
    }

    // Render position capability
    if (state.position) {
      if (state.position.pan !== undefined) {
        const panCh = primitive.fixture.channelNumber(
          ChannelGroup.Pan,
          ChannelByte.MSB,
          primitive.head
        );
        if (panCh !== null) {
          // Map -1..1 to 0..255
          const panValue = ((state.position.pan + 1) / 2) * 255;
          dmx[panCh] = Math.round(panValue);
        }
      }
      if (state.position.tilt !== undefined) {
        const tiltCh = primitive.fixture.channelNumber(
          ChannelGroup.Tilt,
          ChannelByte.MSB,
          primitive.head
        );
        if (tiltCh !== null) {
          const tiltValue = ((state.position.tilt + 1) / 2) * 255;
          dmx[tiltCh] = Math.round(tiltValue);
        }
      }
    }

    // ... similar for strobe, zoom, focus, etc.
  }

  return dmx;
}
```

**Key points:**
- Only primitives in the buffer get rendered
- Missing capabilities are gracefully skipped
- Normalized values (0-1) converted to DMX (0-255)
- Uses QLC+-style capability lookup

---

## Benefits of This Design

### 1. **Separation of Concerns**
- **Pattern logic**: Works in normalized, semantic space
- **Compositing**: Handles blending, layering, z-index
- **DMX rendering**: Handles hardware diversity

### 2. **Hardware Abstraction**
- Patterns don't know about DMX channels
- Fixtures don't know about blend modes
- Compositing doesn't know about hardware

### 3. **Extensibility**
- Add new capabilities (e.g., `iris`, `prism`) without changing compositing
- Add new blend modes without changing patterns or DMX renderer
- Add new fixture types without changing patterns

### 4. **Portability**
- Same composite buffer works on any venue
- DMX renderer adapts to available hardware
- Patterns are venue-agnostic

### 5. **Testability**
- Can test patterns without DMX hardware (inspect composite buffer)
- Can test compositing logic in isolation
- Can verify DMX output against expected values

---

## Performance Considerations

### 1. **Sparse Representation**
Only include capabilities that are actively controlled:
```typescript
// Good: Only controls color
{ primitiveId: "...", color: {...} }

// Bad: Includes all capabilities even if not used
{ primitiveId: "...", color: {...}, intensity: undefined, position: undefined, ... }
```

### 2. **Dirty Tracking**
Track which primitives changed since last frame:
```typescript
interface CompositeBuffer {
  primitives: Map<string, PrimitiveState>;
  dirtyPrimitives: Set<string>;  // Only render these to DMX
}
```

### 3. **Caching**
Cache capability lookups:
```typescript
class PrimitiveCapabilityCache {
  rgbChannels: number[] | null;
  dimmerChannel: number | null;
  panChannel: number | null;
  tiltChannel: number | null;
}
```

### 4. **Batch Updates**
Update DMX universe in one pass, not per-primitive.

---

## Implementation Checklist

- [ ] Define `PrimitiveState` type
- [ ] Define `CompositeBuffer` type
- [ ] Implement blend modes (add, multiply, replace, etc.)
- [ ] Implement layer compositing algorithm
- [ ] Implement DMX renderer with capability lookup
- [ ] Add dirty tracking for performance
- [ ] Add capability cache for fixtures
- [ ] Handle 16-bit channels (pan/tilt fine)
- [ ] Handle HTP vs LTP channel priorities
- [ ] Add frame timing / interpolation

---

## Next Steps

1. **Import QLC+ fixture definitions** into Luma format
2. **Build capability lookup system** (similar to QLC+)
3. **Implement compositing engine** with blend modes
4. **Create DMX renderer** that converts buffer → DMX
5. **Test with diverse fixtures** (Tetra Bar, B-EYE, etc.)
