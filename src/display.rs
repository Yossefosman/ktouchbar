// SPDX-License-Identifier: GPL-3.0-only
use anyhow::{anyhow, Result};
use drm::{
    buffer::DrmFourcc,
    control::{
        atomic, connector,
        dumbbuffer::{DumbBuffer, DumbMapping},
        framebuffer, property, AtomicCommitFlags, ClipRect, Device as ControlDevice, Mode,
        ResourceHandle,
    },
    ClientCapability, Device as DrmDevice,
};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::io::{AsFd, BorrowedFd},
    path::Path,
};

struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl ControlDevice for Card {}
impl DrmDevice for Card {}

impl Card {
    fn open(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        options.write(true);

        Ok(Card(options.open(path)?))
    }
}

pub struct DrmBackend {
    card: Card,
    mode: Mode,
    db: DumbBuffer,
    fb: framebuffer::Handle,
}

impl Drop for DrmBackend {
    fn drop(&mut self) {
        self.card.destroy_framebuffer(self.fb).unwrap();
        self.card.destroy_dumb_buffer(self.db).unwrap();
    }
}

fn find_prop_ids<T: ResourceHandle>(
    card: &Card,
    handle: T,
) -> std::collections::HashMap<String, property::Handle> {
    let mut map = std::collections::HashMap::new();
    if let Ok(props) = card.get_properties(handle) {
        for id in props.as_props_and_values().0 {
            if let Ok(info) = card.get_property(*id) {
                if let Ok(name) = info.name().to_str() {
                    map.insert(name.to_owned(), *id);
                }
            }
        }
    }
    map
}

fn try_open_card(path: &Path) -> Result<DrmBackend> {
    let card = Card::open(path)?;
    card.set_client_capability(ClientCapability::UniversalPlanes, true)?;
    card.set_client_capability(ClientCapability::Atomic, true)?;
    card.acquire_master_lock()?;

    let res = card.resource_handles()?;
    let con = res
        .connectors()
        .iter()
        .flat_map(|con| card.get_connector(*con, true))
        .find(|i| i.state() == connector::State::Connected)
        .ok_or(anyhow!("No connected connectors found"))?;

    let &mode = con.modes().first().ok_or(anyhow!("No modes found"))?;
    let (disp_width, disp_height) = mode.size();
    if disp_height / disp_width < 30 {
        return Err(anyhow!("This does not look like a touchbar"));
    }
    let crtc = res
        .crtcs()
        .iter()
        .flat_map(|crtc| card.get_crtc(*crtc))
        .next()
        .ok_or(anyhow!("No crtcs found"))?;
    let fmt = DrmFourcc::Xrgb8888;
    let db = card.create_dumb_buffer((64, disp_height.into()), fmt, 32)?;

    let fb = card.add_framebuffer(&db, 24, 32)?;
    let plane = *card
        .plane_handles()?
        .first()
        .ok_or(anyhow!("No planes found"))?;

    let con_props = find_prop_ids(&card, con.handle());
    let crtc_props = find_prop_ids(&card, crtc.handle());
    let plane_props = find_prop_ids(&card, plane);

    let mut atomic_req = atomic::AtomicModeReq::new();
    atomic_req.add_property(
        con.handle(),
        con_props["CRTC_ID"],
        property::Value::CRTC(Some(crtc.handle())),
    );
    let blob = card.create_property_blob(&mode)?;

    atomic_req.add_property(
        crtc.handle(),
        crtc_props["MODE_ID"],
        blob,
    );
    atomic_req.add_property(
        crtc.handle(),
        crtc_props["ACTIVE"],
        property::Value::Boolean(true),
    );
    atomic_req.add_property(
        plane,
        plane_props["FB_ID"],
        property::Value::Framebuffer(Some(fb)),
    );
    atomic_req.add_property(
        plane,
        plane_props["CRTC_ID"],
        property::Value::CRTC(Some(crtc.handle())),
    );
    atomic_req.add_property(
        plane,
        plane_props["SRC_X"],
        property::Value::UnsignedRange(0),
    );
    atomic_req.add_property(
        plane,
        plane_props["SRC_Y"],
        property::Value::UnsignedRange(0),
    );
    atomic_req.add_property(
        plane,
        plane_props["SRC_W"],
        property::Value::UnsignedRange((mode.size().0 as u64) << 16),
    );
    atomic_req.add_property(
        plane,
        plane_props["SRC_H"],
        property::Value::UnsignedRange((mode.size().1 as u64) << 16),
    );
    atomic_req.add_property(
        plane,
        plane_props["CRTC_X"],
        property::Value::SignedRange(0),
    );
    atomic_req.add_property(
        plane,
        plane_props["CRTC_Y"],
        property::Value::SignedRange(0),
    );
    atomic_req.add_property(
        plane,
        plane_props["CRTC_W"],
        property::Value::UnsignedRange(mode.size().0 as u64),
    );
    atomic_req.add_property(
        plane,
        plane_props["CRTC_H"],
        property::Value::UnsignedRange(mode.size().1 as u64),
    );

    card.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, atomic_req)?;

    Ok(DrmBackend { card, mode, db, fb })
}

impl DrmBackend {
    pub fn open_card() -> Result<DrmBackend> {
        let mut errors = Vec::new();
        for entry in fs::read_dir("/dev/dri/")? {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with("card") {
                continue;
            }
            match try_open_card(&entry.path()) {
                Ok(card) => return Ok(card),
                Err(err) => errors.push(format!(
                    "{}: {}",
                    entry.path().as_os_str().to_string_lossy(),
                    err
                )),
            }
        }
        Err(anyhow!(
            "No touchbar device found, attempted: [\n    {}\n]",
            errors.join(",\n    ")
        ))
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn fb_info(&self) -> Result<framebuffer::Info> {
        Ok(self.card.get_framebuffer(self.fb)?)
    }
    pub fn dirty(&self, clips: &[ClipRect]) -> Result<()> {
        Ok(self.card.dirty_framebuffer(self.fb, clips)?)
    }
    pub fn map(&mut self) -> Result<DumbMapping<'_>> {
        Ok(self.card.map_dumb_buffer(&mut self.db)?)
    }
}
