//! What can go wrong in the render core, with the stable codes of the
//! CLI's JSON contract (DECISIONS D27).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RenderError {
    /// Vulkan lists no adapter at all: no driver, or no loader.
    #[error(
        "no Vulkan adapter (looked on {}); install Mesa's Vulkan drivers and the Vulkan loader \
         (`spherecut doctor` gives the command for this system)",
        tried.join(", ")
    )]
    NoAdapter { tried: Vec<String> },
    /// No adapter matches the one asked for.
    #[error(
        "no Vulkan adapter matches \"{wanted}\"; there are: {}",
        list(available)
    )]
    AdapterNotFound {
        wanted: String,
        available: Vec<String>,
    },
    /// The adapter wouldn't give a device, or the device refused work.
    #[error("the GPU device: {0}")]
    Device(String),
    /// A picture or a target larger than the device's textures.
    #[error("{w}x{h} is larger than this GPU's {max}x{max} textures")]
    TooLarge { w: u32, h: u32, max: u32 },
    /// A size the renderer can't use: zero, or a plane that doesn't match
    /// its layout.
    #[error("{0}")]
    Invalid(String),
    /// A frame doesn't fit the slot it is uploaded into: another size or
    /// plane layout. A new slot is needed.
    #[error("a {got} frame doesn't fit a slot made for {slot}")]
    WrongSlot { got: String, slot: String },
    /// Copying a picture back from the GPU failed.
    #[error("reading back from the GPU: {0}")]
    Readback(String),
    /// The GPU ran out of memory allocating `what`.
    #[error("the GPU ran out of memory for {0}")]
    OutOfMemory(&'static str),
    /// A decoded picture couldn't be imported (slice 5b): the device has
    /// no DMA-BUF import, or the driver refuses the picture's format,
    /// modifier or size. The copy path still takes it.
    #[error("importing the decoder's picture: {0}")]
    Import(String),
}

impl RenderError {
    /// The error's code in the CLI's JSON contract.
    pub fn code(&self) -> &'static str {
        match self {
            RenderError::NoAdapter { .. } => "no_gpu",
            RenderError::AdapterNotFound { .. } => "adapter_not_found",
            RenderError::Device(_) => "gpu_failed",
            RenderError::TooLarge { .. } => "too_large",
            RenderError::Invalid(_) => "invalid_input",
            RenderError::WrongSlot { .. } => "internal",
            RenderError::Readback(_) => "gpu_failed",
            RenderError::OutOfMemory(_) => "out_of_gpu_memory",
            RenderError::Import(_) => "import_failed",
        }
    }

    /// Every code [`RenderError::code`] gives, for the contract's list.
    pub const CODES: &'static [&'static str] = &[
        "no_gpu",
        "adapter_not_found",
        "gpu_failed",
        "too_large",
        "invalid_input",
        "internal",
        "out_of_gpu_memory",
        "import_failed",
    ];
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_listed_and_messages_say_what_to_do() {
        let errors = [
            RenderError::NoAdapter {
                tried: vec!["Vulkan".into()],
            },
            RenderError::AdapterNotFound {
                wanted: "radeon".into(),
                available: vec!["llvmpipe (LLVM 20.1.2, 256 bits)".into()],
            },
            RenderError::Device("lost".into()),
            RenderError::TooLarge {
                w: 20000,
                h: 10,
                max: 16384,
            },
            RenderError::Invalid("a 0x0 target".into()),
            RenderError::WrongSlot {
                got: "nv12 64x32".into(),
                slot: "nv12 32x16".into(),
            },
            RenderError::Readback("map failed".into()),
            RenderError::OutOfMemory("a slot"),
            RenderError::Import("no DMA-BUF import".into()),
        ];
        for e in &errors {
            assert!(RenderError::CODES.contains(&e.code()), "{e:?}");
        }
        assert_eq!(errors[0].code(), "no_gpu");
        assert!(errors[0].to_string().contains("spherecut doctor"));
        assert!(errors[1].to_string().contains("llvmpipe"));
        assert_eq!(
            errors[3].to_string(),
            "20000x10 is larger than this GPU's 16384x16384 textures"
        );
        assert_eq!(errors[3].code(), "too_large");
    }
}
