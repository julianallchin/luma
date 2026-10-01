use crate::*;

pub(crate) fn primitive(p: Primitive) -> Definition {
    match p {
        Primitive::Kernel(kernel) => kernel.definition(),
        Primitive::Output => crate::output::terminal_definition(),
        Primitive::BandEnergy => crate::features::definition(),
    }
}

/// The kernels a clip graph lowers onto, plus band energy and the output
/// terminal.
pub fn standard_library() -> Library {
    static LIBRARY: std::sync::OnceLock<Library> = std::sync::OnceLock::new();
    LIBRARY
        .get_or_init(|| {
            let mut library = Library::default();
            for (id, op) in [
                ("band_energy", Primitive::BandEnergy),
                ("output", Primitive::Output),
            ] {
                library.definitions.insert(id.into(), primitive(op));
            }
            for kernel in crate::clip_graph::Kernel::ALL {
                library
                    .definitions
                    .insert(kernel.id().into(), primitive(Primitive::Kernel(kernel)));
            }
            library
        })
        .clone()
}
