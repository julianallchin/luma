//! Draw a subtree at another rem: the one way to scale a tree of real
//! elements in gpui, which has no transforms. Every [`crate::rpx`] length,
//! text size included, resolves against the rem in force when it is laid
//! out, so a subtree drawn under a larger rem is magnified whole. Painted
//! code inside reads the factor with [`crate::rem_scale`].

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Window,
};

/// `child`, laid out, prepainted and painted with the rem at `rem`.
pub fn rem_scaled(rem: Pixels, child: impl IntoElement) -> RemScaled {
    RemScaled {
        rem,
        child: child.into_any_element(),
    }
}

pub struct RemScaled {
    rem: Pixels,
    child: AnyElement,
}

impl IntoElement for RemScaled {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for RemScaled {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = &mut self.child;
        let layout =
            window.with_rem_size(Some(self.rem), |window| child.request_layout(window, cx));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| child.paint(window, cx));
    }
}
