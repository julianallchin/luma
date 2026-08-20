//! The automation node tree: what a machine can see of this UI.
//!
//! GPUI paints its own widgets, so there is no OS control to query — a driver
//! that wants to click "Back" has no way to find "Back". This module is the
//! answer: every control that matters names itself during `prepaint`, when its
//! bounds are known, into a [`NodeRegistry`] that lives for exactly one frame.
//! `gpui-agent` reads that registry and turns a name into a click point.
//!
//! # Interface
//!
//! ```ignore
//! use luma_ui::node::{Instrument, Role};
//!
//! luma_ui::luma_button("Back", false)
//!     .id("back")
//!     .on_click(…)
//!     .agent_node(Role::Button, "Back")   // closes the chain
//! ```
//!
//! [`Instrument::agent_node`] wraps the element, so it must come **last**:
//! `Instrumented` is an [`Element`], not a `Div`, and there is no `.id()` or
//! `.on_click()` on the far side of it.
//!
//! # Cost when off
//!
//! Without the `agent` feature `agent_node` is the identity function and this
//! module holds no state. Call sites do not need to be `cfg`'d, and a release
//! build of the app carries nothing from here.
//!
//! # Why not gpui's accessibility tree
//!
//! gpui builds an AccessKit tree during `prepaint` that carries roles, labels
//! and bounds — on paper exactly this. It is unusable here: the tree is only
//! built when `Window::a11y.is_active()`, and that flag is set by a
//! platform adapter that `TestPlatform` does not implement, so under the
//! headless harness it is always false and the tree is always empty. The
//! shapes below are deliberately a11y-shaped so that annotating these same
//! call sites with `.role()` later is a widening, not a rewrite.

use gpui::{Bounds, IntoElement, Pixels, SharedString};

/// What kind of thing a node is. A closed vocabulary on purpose: a driver
/// filters on `role` and every new variant is a new thing scripts must learn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Button,
    Toggle,
    Checkbox,
    Input,
    Select,
    Slider,
    /// A row in a list or table.
    Row,
    /// A pressable panel — a venue card, a tile.
    Card,
    /// Read-only text a script may want to assert on.
    Text,
}

impl Role {
    /// The wire spelling, and the one scripts match against.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::Toggle => "toggle",
            Self::Checkbox => "checkbox",
            Self::Input => "input",
            Self::Select => "select",
            Self::Slider => "slider",
            Self::Row => "row",
            Self::Card => "card",
            Self::Text => "text",
        }
    }

    /// Parse a wire spelling. Used by the harness to match a stale node
    /// against a fresh frame.
    ///
    /// Not `FromStr`: an unknown role is a normal outcome for a script that
    /// typed one, not an error worth a type of its own.
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Button,
            Self::Toggle,
            Self::Checkbox,
            Self::Input,
            Self::Select,
            Self::Slider,
            Self::Row,
            Self::Card,
            Self::Text,
        ]
        .into_iter()
        .find(|role| role.as_str() == name)
    }
}

/// One control, as it existed in one frame.
#[derive(Debug, Clone)]
pub struct Node {
    /// Position in this frame's registration order. Frame-scoped: the same
    /// control may have a different id next frame, which is why every
    /// mutating call carries the frame back (see `gpui_agent::protocol`).
    pub id: usize,
    pub role: Role,
    pub label: SharedString,
    /// Window-space bounds, clipped to the content mask. Empty when the
    /// control is scrolled or masked out of view — there is then no point on
    /// screen that would hit it, and the harness refuses to click it.
    pub bounds: Bounds<Pixels>,
    /// Whether the control would accept input if you hit it. Deliberately not
    /// derived from `bounds`: "disabled" and "off screen" are different facts
    /// and a script wants to tell them apart.
    pub enabled: bool,
    pub focused: bool,
}

/// Attach automation identity to an element.
pub trait Instrument: IntoElement + Sized {
    /// The element this call produces — `Instrumented<Self::Element>` with the
    /// `agent` feature on, `Self` without it.
    type Output: AgentNode;

    /// Name this element in the automation tree. Must be the **last** call in
    /// a builder chain.
    fn agent_node(self, role: Role, label: impl Into<SharedString>) -> Self::Output;
}

/// The far side of [`Instrument::agent_node`]: still an element, and still
/// carrying the two facts a control has beyond its identity.
pub trait AgentNode: IntoElement + Sized {
    /// Mark this control as not accepting input. Reported as `enabled: false`.
    fn agent_disabled(self, disabled: bool) -> Self;

    /// Report this control as focused. gpui routes keystrokes and actions
    /// along the path to the focused element, so this is what tells a script
    /// where `app.key()` will land.
    fn agent_focused(self, focused: bool) -> Self;
}

#[cfg(not(feature = "agent"))]
mod imp {
    use super::{AgentNode, Instrument, Role};
    use gpui::{IntoElement, SharedString};

    impl<E: IntoElement> Instrument for E {
        type Output = E;

        #[inline(always)]
        fn agent_node(self, _role: Role, _label: impl Into<SharedString>) -> E {
            self
        }
    }

    impl<E: IntoElement> AgentNode for E {
        #[inline(always)]
        fn agent_disabled(self, _disabled: bool) -> Self {
            self
        }

        #[inline(always)]
        fn agent_focused(self, _focused: bool) -> Self {
            self
        }
    }
}

#[cfg(feature = "agent")]
pub use imp::{Instrumented, NodeRegistry};

#[cfg(feature = "agent")]
mod imp {
    use super::{Instrument, Node, Role};
    use gpui::{
        App, Bounds, Element, ElementId, Global, GlobalElementId, Hitbox, HitboxBehavior,
        InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Window,
    };

    /// Every node registered so far this frame.
    ///
    /// A `Global` rather than window state because the reader — the harness —
    /// holds an `&App` and no window, and because a single window is the only
    /// shape this app has. Two windows would need this keyed by handle.
    #[derive(Default)]
    pub struct NodeRegistry {
        frame: u64,
        nodes: Vec<Node>,
    }

    impl Global for NodeRegistry {}

    impl NodeRegistry {
        /// Which frame the current [`Self::nodes`] belong to. Monotonic across
        /// the life of the app; every draw bumps it exactly once.
        pub fn frame(&self) -> u64 {
            self.frame
        }

        pub fn nodes(&self) -> &[Node] {
            &self.nodes
        }

        /// Start a new frame: drop the previous one's nodes and bump the
        /// counter. Called from [`Instrumented::request_layout`] on the root,
        /// which is the first thing gpui runs in a draw and therefore the one
        /// place that is guaranteed to be "before every prepaint".
        fn begin_frame(cx: &mut App) {
            let registry = cx.default_global::<NodeRegistry>();
            registry.frame += 1;
            registry.nodes.clear();
        }

        fn push(cx: &mut App, node: impl FnOnce(usize) -> Node) {
            let registry = cx.default_global::<NodeRegistry>();
            let id = registry.nodes.len();
            let node = node(id);
            registry.nodes.push(node);
        }
    }

    /// An element that names itself in the [`NodeRegistry`] every frame.
    ///
    /// Layout is entirely the wrapped element's: this delegates
    /// `request_layout` and so shares its `LayoutId`, which is what makes the
    /// `bounds` handed to `prepaint` the wrapped element's own box.
    pub struct Instrumented<E> {
        element: E,
        role: Role,
        label: SharedString,
        enabled: bool,
        focused: bool,
        /// Set on the root wrapper only — see [`NodeRegistry::begin_frame`].
        root: bool,
    }

    impl<E: Element> Instrumented<E> {
        pub(super) fn new(element: E, role: Role, label: SharedString) -> Self {
            Self {
                element,
                role,
                label,
                enabled: true,
                focused: false,
                root: false,
            }
        }

        /// Mark this wrapper as the frame boundary. There must be exactly one
        /// per window, above everything else — [`crate::node::agent_root`].
        fn into_root(mut self) -> Self {
            self.root = true;
            self
        }
    }

    impl<E: Element> Element for Instrumented<E> {
        type RequestLayoutState = E::RequestLayoutState;
        type PrepaintState = (Option<Hitbox>, E::PrepaintState);

        fn id(&self) -> Option<ElementId> {
            self.element.id()
        }

        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            self.element.source_location()
        }

        fn request_layout(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            window: &mut Window,
            cx: &mut App,
        ) -> (LayoutId, Self::RequestLayoutState) {
            if self.root {
                NodeRegistry::begin_frame(cx);
            }
            self.element.request_layout(id, inspector_id, window, cx)
        }

        fn prepaint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            request_layout: &mut Self::RequestLayoutState,
            window: &mut Window,
            cx: &mut App,
        ) -> Self::PrepaintState {
            // The root wrapper is the frame boundary, not a control; giving it
            // a hitbox would put a node above every real one for no reader.
            let hitbox = (!self.root).then(|| {
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                let role = self.role;
                let label = self.label.clone();
                let (enabled, focused) = (self.enabled, self.focused);
                // A control reaches only as far as its content mask: a
                // virtualized row scrolled past the viewport still prepaints,
                // and clicking its nominal bounds would hit whatever is really
                // there. An empty intersection means there is nothing to click.
                let bounds = hitbox.bounds.intersect(&hitbox.content_mask.bounds);
                NodeRegistry::push(cx, |id| Node {
                    id,
                    role,
                    label,
                    bounds,
                    enabled,
                    focused,
                });
                hitbox
            });
            let inner = self
                .element
                .prepaint(id, inspector_id, bounds, request_layout, window, cx);
            (hitbox, inner)
        }

        fn paint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            request_layout: &mut Self::RequestLayoutState,
            prepaint: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            self.element.paint(
                id,
                inspector_id,
                bounds,
                request_layout,
                &mut prepaint.1,
                window,
                cx,
            );
        }
    }

    impl<E: Element> IntoElement for Instrumented<E> {
        type Element = Self;

        fn into_element(self) -> Self {
            self
        }
    }

    impl<E: IntoElement> Instrument for E {
        type Output = Instrumented<E::Element>;

        fn agent_node(self, role: Role, label: impl Into<SharedString>) -> Self::Output {
            Instrumented::new(self.into_element(), role, label.into())
        }
    }

    impl<E: Element> super::AgentNode for Instrumented<E> {
        fn agent_disabled(mut self, disabled: bool) -> Self {
            self.enabled = !disabled;
            self
        }

        fn agent_focused(mut self, focused: bool) -> Self {
            self.focused = focused;
            self
        }
    }

    /// Wrap a window's whole element tree so that each draw starts a fresh
    /// frame of nodes. Exactly one of these per window, outside everything.
    pub fn agent_root(element: impl IntoElement) -> impl IntoElement {
        Instrumented::new(element.into_element(), Role::Text, SharedString::default()).into_root()
    }
}

/// Wrap a window's whole element tree so that each draw starts a fresh frame of
/// nodes. Without the `agent` feature this is the identity.
#[cfg(feature = "agent")]
pub use imp::agent_root;

#[cfg(not(feature = "agent"))]
#[inline(always)]
pub fn agent_root(element: impl IntoElement) -> impl IntoElement {
    element
}
