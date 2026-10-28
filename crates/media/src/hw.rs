//! Which GPU hardware decoding runs on (M1 plan section 3.5, DECISIONS
//! D30): the DRM render node of the GPU that wgpu renders on, so that a
//! frame is decoded where it is drawn. Decoding on one GPU and rendering
//! on another would add a copy over PCIe and rule out zero-copy (D25), as
//! on a machine with an integrated GPU beside a discrete one.
//!
//! The lookup reads sysfs, under a root that tests can fake:
//!
//! 1. the PCI bus id wgpu gives its adapter (`AdapterInfo::device_pci_bus_id`,
//!    `"0000:03:00.0"`), as `<sys>/bus/pci/devices/<id>/drm/renderD*`;
//! 2. else the adapter's PCI vendor and device ids, matched against
//!    `<sys>/class/drm/renderD*/device/{vendor,device}`;
//! 3. else `/dev/dri/renderD128`.
//!
//! wgpu arrives in slice 3; until then callers know no adapter and get
//! the last step, and `--render-node` overrides the lookup.

use std::path::{Path, PathBuf};

/// The render node used when nothing says which GPU to decode on.
pub const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";

/// What wgpu says about the adapter it renders on (`wgpu::AdapterInfo`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GpuIds {
    /// `device_pci_bus_id`, as `"{:04x}:{:02x}:{:02x}.{}"`; empty when
    /// unknown.
    pub pci_bus_id: String,
    /// The PCI vendor and device ids; 0 when unknown.
    pub vendor: u32,
    pub device: u32,
}

/// Why [`Sysfs::render_node`] chose the node it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeChoice {
    /// The caller gave it (`--render-node`).
    Given,
    /// It is the render node of the PCI device at this bus id.
    BusId(String),
    /// It is the first render node of a device with these PCI ids.
    Ids { vendor: u32, device: u32 },
    /// Nothing named a GPU that has one: [`DEFAULT_RENDER_NODE`].
    Default,
}

/// A DRM render node, and why it is the one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderNode {
    pub path: PathBuf,
    pub chosen: NodeChoice,
}

impl std::fmt::Display for RenderNode {
    /// e.g. "/dev/dri/renderD129, the GPU at PCI 0000:03:00.0".
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.display())?;
        match &self.chosen {
            NodeChoice::Given => write!(f, ", as given"),
            NodeChoice::BusId(id) => write!(f, ", the GPU at PCI {id}"),
            NodeChoice::Ids { vendor, device } => {
                write!(f, ", the first GPU with PCI ids {vendor:04x}:{device:04x}")
            }
            NodeChoice::Default => write!(f, ", the default"),
        }
    }
}

/// Where sysfs and the DRM device nodes are: `/sys` and `/dev/dri`, or a
/// test's fake tree.
#[derive(Debug, Clone)]
pub struct Sysfs {
    sys: PathBuf,
    dev_dri: PathBuf,
}

impl Sysfs {
    /// The machine's own.
    pub fn system() -> Self {
        Sysfs::at("/sys", "/dev/dri")
    }

    /// sysfs at `sys` and the DRM nodes in `dev_dri`.
    pub fn at(sys: impl Into<PathBuf>, dev_dri: impl Into<PathBuf>) -> Self {
        Sysfs {
            sys: sys.into(),
            dev_dri: dev_dri.into(),
        }
    }

    /// The render node to decode on: `given`, else the one of the GPU that
    /// `gpu` names (by PCI bus id, then by vendor and device ids), else
    /// [`DEFAULT_RENDER_NODE`]. Nothing is opened, so the node may not
    /// exist; opening a device on it says so.
    pub fn render_node(&self, gpu: Option<&GpuIds>, given: Option<&Path>) -> RenderNode {
        spherecut_core::may_block("reading the GPUs' render nodes in sysfs");
        if let Some(path) = given {
            return RenderNode {
                path: path.to_path_buf(),
                chosen: NodeChoice::Given,
            };
        }
        if let Some(gpu) = gpu {
            let id = gpu.pci_bus_id.trim().to_ascii_lowercase();
            if let Some(name) = self.by_bus_id(&id) {
                return RenderNode {
                    path: self.dev_dri.join(name),
                    chosen: NodeChoice::BusId(id),
                };
            }
            if let Some(name) = self.by_ids(gpu.vendor, gpu.device) {
                return RenderNode {
                    path: self.dev_dri.join(name),
                    chosen: NodeChoice::Ids {
                        vendor: gpu.vendor,
                        device: gpu.device,
                    },
                };
            }
        }
        RenderNode {
            path: PathBuf::from(DEFAULT_RENDER_NODE),
            chosen: NodeChoice::Default,
        }
    }

    /// The render node of the PCI device at bus id `id`.
    fn by_bus_id(&self, id: &str) -> Option<String> {
        // A bus id is hex digits, colons and a dot; anything else (an
        // empty one, a path) names no device.
        let valid = !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
            && !id.contains("..");
        if !valid {
            return None;
        }
        let dir = self.sys.join("bus/pci/devices").join(id).join("drm");
        render_nodes(&dir).into_iter().next().map(|(_, name)| name)
    }

    /// The first render node, by number, of a device with PCI ids
    /// `vendor` and `device`. Both must be known: two GPUs of one vendor
    /// (an AMD iGPU beside an AMD card) differ only by the device id.
    fn by_ids(&self, vendor: u32, device: u32) -> Option<String> {
        if vendor == 0 || device == 0 {
            return None;
        }
        let class = self.sys.join("class/drm");
        render_nodes(&class).into_iter().find_map(|(_, name)| {
            let dev = class.join(&name).join("device");
            (read_id(&dev.join("vendor")) == Some(vendor)
                && read_id(&dev.join("device")) == Some(device))
            .then_some(name)
        })
    }
}

/// The lookup on this machine's sysfs (see [`Sysfs::render_node`]).
pub fn render_node(gpu: Option<&GpuIds>, given: Option<&Path>) -> RenderNode {
    Sysfs::system().render_node(gpu, given)
}

/// The `renderD<n>` entries of `dir`, by number.
fn render_nodes(dir: &Path) -> Vec<(u32, String)> {
    let mut nodes: Vec<(u32, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            let n = name.strip_prefix("renderD")?.parse().ok()?;
            Some((n, name))
        })
        .collect();
    nodes.sort();
    nodes
}

/// A sysfs id file, such as `vendor`: `0x1002` and a newline.
fn read_id(path: &Path) -> Option<u32> {
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A fake sysfs tree in the system's temp dir, removed when dropped.
    struct Tree(PathBuf);

    impl Tree {
        /// As on a desktop with an AMD iGPU (renderD128, at 0000:0e:00.0)
        /// and an AMD card (renderD129, at 0000:03:00.0), plus a node of
        /// another vendor's card with no PCI directory.
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("spherecut-hw-{}-{name}", std::process::id()));
            let t = Tree(root);
            for (card, node, bus, vendor, device) in [
                ("card0", "renderD128", "0000:0e:00.0", "0x1002", "0x15bf"),
                ("card1", "renderD129", "0000:03:00.0", "0x1002", "0x7480"),
                ("card2", "renderD130", "", "0x8086", "0x7d51"),
            ] {
                let dev = t.0.join("sys/class/drm").join(node).join("device");
                std::fs::create_dir_all(&dev).unwrap();
                std::fs::write(dev.join("vendor"), format!("{vendor}\n")).unwrap();
                std::fs::write(dev.join("device"), format!("{device}\n")).unwrap();
                if !bus.is_empty() {
                    let drm = t.0.join("sys/bus/pci/devices").join(bus).join("drm");
                    std::fs::create_dir_all(drm.join(node)).unwrap();
                    std::fs::create_dir_all(drm.join(card)).unwrap();
                }
            }
            t
        }

        fn sysfs(&self) -> Sysfs {
            Sysfs::at(self.0.join("sys"), "/dev/dri")
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn gpu(bus: &str, vendor: u32, device: u32) -> GpuIds {
        GpuIds {
            pci_bus_id: bus.into(),
            vendor,
            device,
        }
    }

    #[test]
    fn the_render_node_is_found_by_bus_id_then_ids_then_the_default() {
        let t = Tree::new("lookup");
        let fs = t.sysfs();
        let node = |g: Option<&GpuIds>| fs.render_node(g, None);
        // The card by its bus id, whatever its ids say (in upper case too).
        let n = node(Some(&gpu("0000:03:00.0", 0, 0)));
        assert_eq!(n.path, Path::new("/dev/dri/renderD129"));
        assert_eq!(n.chosen, NodeChoice::BusId("0000:03:00.0".into()));
        assert_eq!(
            n.to_string(),
            "/dev/dri/renderD129, the GPU at PCI 0000:03:00.0"
        );
        let n = node(Some(&gpu("0000:0E:00.0", 0x1002, 0x7480)));
        assert_eq!(n.path, Path::new("/dev/dri/renderD128"));
        // A bus id with no device: the ids find the card, not the iGPU of
        // the same vendor.
        let n = node(Some(&gpu("0000:07:00.0", 0x1002, 0x7480)));
        assert_eq!(n.path, Path::new("/dev/dri/renderD129"));
        assert_eq!(
            n.chosen,
            NodeChoice::Ids {
                vendor: 0x1002,
                device: 0x7480
            }
        );
        // No bus id at all, as some drivers give none, and a node with no
        // PCI directory.
        let n = node(Some(&gpu("", 0x8086, 0x7d51)));
        assert_eq!(n.path, Path::new("/dev/dri/renderD130"));
        // An unknown device id doesn't settle for the vendor alone, and no
        // adapter at all: the default.
        for g in [
            Some(gpu("", 0x1002, 0)),
            Some(gpu("", 0x10de, 0x2684)),
            Some(gpu("../../..", 0x10de, 0x2684)),
            None,
        ] {
            let n = node(g.as_ref());
            assert_eq!(n.path, Path::new(DEFAULT_RENDER_NODE), "{g:?}");
            assert_eq!(n.chosen, NodeChoice::Default);
        }
        // Given wins over everything.
        let given = Path::new("/nonexistent/renderD200");
        let n = fs.render_node(Some(&gpu("0000:03:00.0", 0, 0)), Some(given));
        assert_eq!((n.path.as_path(), n.chosen), (given, NodeChoice::Given));
    }

    #[test]
    fn without_sysfs_the_default_is_used() {
        let fs = Sysfs::at("/nonexistent/sys", "/dev/dri");
        let n = fs.render_node(Some(&gpu("0000:03:00.0", 0x1002, 0x7480)), None);
        assert_eq!(n.chosen, NodeChoice::Default);
    }

    #[test]
    fn ids_are_read_as_sysfs_writes_them() {
        let t = Tree::new("ids");
        let f = t.0.join("id");
        for (text, want) in [
            ("0x1002\n", Some(0x1002)),
            ("0X10DE", Some(0x10de)),
            ("1002\n", None),
            ("", None),
        ] {
            std::fs::write(&f, text).unwrap();
            assert_eq!(read_id(&f), want, "{text:?}");
        }
    }
}
