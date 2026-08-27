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
//! ```text
//! use luma_ui::node::{Instrument, Role};
//! use luma_ui::Enabled;
//!
//! luma_ui::luma_button("Back", Enabled::Yes)
//!     .id("back")
//!     .on_click(…)
//!     .agent_node(Role::Button, "Back")   // closes the chain
//! ```
//!
//! A control the app *paints* rather than lays out — a card on a custom canvas
//! — has no element to wrap, and registers itself by hand instead:
//!
//! ```text
//! luma_ui::node::agent_paint_node(Role::Card, title, card_bounds, window, cx);
//! ```
//!
//! [`Instrument::agent_node`] wraps the element, so it must come **last**:
//! `Instrumented` is a [`gpui::Element`], not a `Div`, and there is no `.id()` or
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
    /// A labelled state pill — a tool call in the agent chat.
    ///
    /// Its own role because a chip is none of the others: not a button (it is
    /// not pressable at rest), not text (it carries state), not a row. A
    /// script that had to assert on a `Text` node would be asserting on a
    /// phrasing detail instead of on the thing.
    Chip,
}

impl Role {
    /// Every role, once. [`Role::parse`] is derived from this rather than
    /// re-listing the variants, so the vocabulary a script can name and the
    /// vocabulary a node can carry cannot drift apart. `gpui-agent`'s
    /// `the_declared_roles_and_the_registry_are_the_same_set` holds this
    /// against the `Role` union in `api.d.ts` in both directions, count
    /// included — which is what catches a variant added here and forgotten
    /// there.
    pub const ALL: &'static [Self] = &[
        Self::Button,
        Self::Toggle,
        Self::Checkbox,
        Self::Input,
        Self::Select,
        Self::Slider,
        Self::Row,
        Self::Card,
        Self::Text,
        Self::Chip,
    ];

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
            Self::Chip => "chip",
        }
    }

    /// Parse a wire spelling. Used by the harness to match a stale node
    /// against a fresh frame.
    ///
    /// Not `FromStr`: an unknown role is a normal outcome for a script that
    /// typed one, not an error worth a type of its own.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|role| role.as_str() == name)
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

/// Name a control that has no element of its own.
///
/// [`Instrument::agent_node`] can only speak for things gpui laid out. A
/// custom-painted surface — the pattern graph's canvas — draws its controls
/// itself, so nothing in the element tree has a node card's bounds. Pass the
/// window-space box you are about to paint into and it is registered the same
/// way, clipped here against the window's current content mask.
///
/// The clip is not the caller's job. A node whose `bounds` overstate where it
/// really is does not fail loudly: the harness clicks the middle of them and
/// hits whatever is actually masked in at that point. `resolve` in
/// `gpui_agent::pump` leans on empty bounds meaning "nothing to click", so the
/// one place that can guarantee it is the one doing the registering.
///
/// Must be called during `prepaint`, alongside every other registration, so
/// the frame's ids stay in tree order. `Window::content_mask` asserts that for
/// us.
#[cfg(feature = "agent")]
pub fn agent_paint_node(
    role: Role,
    label: impl Into<SharedString>,
    bounds: Bounds<Pixels>,
    window: &gpui::Window,
    cx: &mut gpui::App,
) {
    agent_paint_node_focused(role, label, bounds, false, window, cx);
}

/// [`agent_paint_node`], carrying the painted twin of
/// [`AgentNode::agent_focused`]: a painted control the screen's keyboard verbs
/// will act on next — a selected card on the graph canvas — reports
/// `focused: true`, which is how a script asserts a selection the paint only
/// shows as a border.
#[cfg(feature = "agent")]
pub fn agent_paint_node_focused(
    role: Role,
    label: impl Into<SharedString>,
    bounds: Bounds<Pixels>,
    focused: bool,
    window: &gpui::Window,
    cx: &mut gpui::App,
) {
    let bounds = bounds.intersect(&window.content_mask().bounds);
    imp::push_painted(cx, role, label.into(), bounds, focused);
}

/// See the `agent`-enabled twin. Without the feature there is no registry, so
/// this is the identity on nothing.
#[cfg(not(feature = "agent"))]
#[inline(always)]
pub fn agent_paint_node(
    _role: Role,
    _label: impl Into<SharedString>,
    _bounds: Bounds<Pixels>,
    _window: &gpui::Window,
    _cx: &mut gpui::App,
) {
}

/// See the `agent`-enabled twin.
#[cfg(not(feature = "agent"))]
#[inline(always)]
pub fn agent_paint_node_focused(
    _role: Role,
    _label: impl Into<SharedString>,
    _bounds: Bounds<Pixels>,
    _focused: bool,
    _window: &gpui::Window,
    _cx: &mut gpui::App,
) {
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

    /// See [`super::agent_paint_node`], which has already clipped `bounds`.
    /// No hitbox: a painted control was never laid out, so there is nothing
    /// for gpui to hit-test against.
    pub(super) fn push_painted(
        cx: &mut App,
        role: Role,
        label: SharedString,
        bounds: Bounds<Pixels>,
        focused: bool,
    ) {
        NodeRegistry::push(cx, |id| Node {
            id,
            role,
            label,
            bounds,
            enabled: true,
            focused,
        });
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
