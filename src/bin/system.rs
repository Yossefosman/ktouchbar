// SPDX-License-Identifier: GPL-3.0-only
use anyhow::Result;
use cairo::{Format, ImageSurface};
use drm::control::ClipRect;
use input::{
    event::{
        device::DeviceEvent,
        keyboard::{KeyState, KeyboardEvent, KeyboardEventTrait},
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot},
        Event, EventTrait,
    },
    Device as InputDevice, Libinput, LibinputInterface,
};
use input_linux::{uinput::UInputHandle, EventKind, Key, SynchronizeKind};
use input_linux_sys::{input_event, input_id, timeval, uinput_setup, KEY_CNT};
use libc::{c_char, O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY};
use nix::sys::{
    epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags},
    signal::{SigSet, Signal},
};
use std::{
    cmp::min,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Read,
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
        unix::fs::OpenOptionsExt,
    },
    panic::{self, AssertUnwindSafe},
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use zbus::{dbus_interface, zvariant};

use ktouchbar::config::Config;
use ktouchbar::display::DrmBackend;
use ktouchbar::display::backlight::BacklightManager;
use ktouchbar::widget::FunctionLayer;

const DE_CHECK_INTERVAL: Duration = Duration::from_secs(2);

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;
        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        _ = File::from(fd);
    }
}

fn emit<F: AsRawFd>(uinput: &UInputHandle<F>, ty: EventKind, code: u16, value: i32) {
    uinput
        .write(&[input_event {
            value,
            type_: ty as u16,
            code,
            time: timeval { tv_sec: 0, tv_usec: 0 },
        }])
        .unwrap();
}

fn toggle_key<F: AsRawFd>(uinput: &UInputHandle<F>, code: Key, value: i32) {
    emit(uinput, EventKind::Key, code as u16, value);
    emit(uinput, EventKind::Synchronize, SynchronizeKind::Report as u16, 0);
}

#[derive(Clone, Debug, serde::Serialize, zvariant::Type)]
struct TouchEventRecord {
    slot: u32,
    x: f64,
    y: f64,
    event_type: u32,
}

struct HardwareState {
    fb_width: u32,
    fb_height: u32,
    mode_hdisplay: u32,
    mode_vdisplay: u32,
    frame_pixels: Vec<u8>,
    frame_count: u32,
    pending_frame: bool,
    pending_touches: Vec<TouchEventRecord>,
    fn_pressed: bool,
}

struct HardwareService {
    state: Arc<Mutex<HardwareState>>,
    backlight: Arc<Mutex<BacklightManager>>,
    uinput: Arc<Mutex<UInputHandle<File>>>,
    event_fd: i32,
}

#[dbus_interface(name = "org.ktouchbar.Hardware")]
impl HardwareService {
    fn push_frame(&mut self, fd: zvariant::Fd) -> zbus::fdo::Result<u32> {
        let owned = unsafe { <OwnedFd as FromRawFd>::from_raw_fd(fd.as_raw_fd()) };
        let mut file = File::from(owned);
        let mut state = self.state.lock().unwrap();
        let size = (state.fb_width * state.fb_height * 4) as usize;
        if state.frame_pixels.len() != size {
            state.frame_pixels.resize(size, 0);
        }
        file.read_exact(&mut state.frame_pixels).map_err(|e| {
            zbus::fdo::Error::Failed(format!("failed to read frame: {e}"))
        })?;
        state.frame_count = state.frame_count.wrapping_add(1);
        state.pending_frame = true;
        let val: u64 = 1;
        unsafe { libc::write(self.event_fd, &val as *const _ as *const libc::c_void, 8); }
        Ok(state.frame_count)
    }

    fn get_display_info(&self) -> (u32, u32, u32, u32) {
        let state = self.state.lock().unwrap();
        (state.fb_width, state.fb_height, state.mode_hdisplay, state.mode_vdisplay)
    }

    fn poll_events(&mut self) -> Vec<TouchEventRecord> {
        std::mem::take(&mut self.state.lock().unwrap().pending_touches)
    }

    fn get_fn_state(&self) -> bool {
        self.state.lock().unwrap().fn_pressed
    }

    fn set_backlight_config(&self, active: u32, dimmed: u32, dim_timeout_ms: u32, off_timeout_ms: u32, adaptive: bool) {
        self.backlight.lock().unwrap().set_config(active, dimmed, dim_timeout_ms, off_timeout_ms, adaptive);
    }

    fn reset_backlight_inactivity(&self) {
        self.backlight.lock().unwrap().reset_activity();
    }

    fn inject_key(&self, key: u32, pressed: bool) {
        let uinput = self.uinput.lock().unwrap();
        emit(&uinput, EventKind::Key, key as u16, pressed as i32);
        emit(&uinput, EventKind::Synchronize, SynchronizeKind::Report as u16, 0);
    }

    fn notify_user_disconnect(&mut self) -> zbus::fdo::Result<()> {
        let val: u64 = 1;
        unsafe { libc::write(self.event_fd, &val as *const _ as *const libc::c_void, 8); }
        Ok(())
    }
}

fn main() {
    let mut drm = match DrmBackend::open_card() {
        Ok(drm) => drm,
        Err(e) => {
            eprintln!(
                "ktouchbar-system: {}\n\
                 ktouchbar-system: if this happened after suspend/resume, this is a known issue\n\
                 ktouchbar-system: with the appletbdrm kernel driver. Rebooting fixes it.\n\
                 ktouchbar-system: hint: ensure your user has CAP_SYS_ADMIN or run as root\n\
                 ktouchbar-system: hint: try: sudo setcap cap_sys_admin+ep /usr/bin/ktouchbar-system",
                e
            );
            std::process::exit(1);
        }
    };
    let (height, width) = drm.mode().size();
    if let Err(e) = panic::catch_unwind(AssertUnwindSafe(|| real_main(&mut drm))) {
        let msg = if let Some(s) = e.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = e.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown".to_string()
        };
        eprintln!("ktouchbar-system: daemon crashed: {msg}");
        eprintln!("ktouchbar-system: touchbar will show red screen. Restart the service to recover.");
        let mut map = drm.map().unwrap();
        let data = map.as_mut();
        for pixel in data.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0x00, 0x00, 0x30, 0xFF]);
        }
        drop(map);
        let _ = drm.dirty(&[ClipRect::new(0, 0, height, width)]);
    }
    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGTERM);
    sigset.wait().unwrap();
}

fn real_main(drm: &mut DrmBackend) {
    let (height, width) = drm.mode().size();
    let (db_width, db_height) = drm.fb_info().unwrap().size();

    let uinput_file = loop {
        match OpenOptions::new().write(true).open("/dev/uinput") {
            Ok(f) => break f,
            Err(e) => {
                eprintln!("ktouchbar-system: failed to open /dev/uinput: {e}, retrying in 1s...");
                thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let uinput = UInputHandle::new(uinput_file);
    uinput.set_evbit(EventKind::Key).unwrap();
    for code in 0u16..KEY_CNT as u16 {
        if let Ok(key) = Key::try_from(code) {
            let _ = uinput.set_keybit(key);
        }
    }

    let mut dev_name = [0 as c_char; 80];
    let dev_name_str = "KTouchBar Virtual Keyboard".as_bytes();
    for i in 0..dev_name_str.len() {
        dev_name[i] = dev_name_str[i] as c_char;
    }
    uinput
        .dev_setup(&uinput_setup {
            id: input_id {
                bustype: 0x19,
                vendor: 0x1209,
                product: 0x316E,
                version: 1,
            },
            ff_effects_max: 0,
            name: dev_name,
        })
        .unwrap();
    uinput.dev_create().unwrap();

    let uinput_shared = Arc::new(Mutex::new(uinput));
    let backlight = Arc::new(Mutex::new(BacklightManager::new()));

    let hw_state = Arc::new(Mutex::new(HardwareState {
        fb_width: db_width,
        fb_height: db_height,
        mode_hdisplay: height as u32,
        mode_vdisplay: width as u32,
        frame_pixels: Vec::new(),
        frame_count: 0,
        pending_frame: false,
        pending_touches: Vec::new(),
        fn_pressed: false,
    }));

    let dbus_conn = match zbus::blocking::Connection::system() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ktouchbar-system: failed to connect to system bus: {e}");
            eprintln!("ktouchbar-system: running in fallback-only mode");
            run_fallback_only(drm, width, height, db_width, db_height);
            return;
        }
    };

    let cfg = Config::fallback_defaults();
    let bl_main = backlight.clone();
    let uinput_fb = uinput_shared.clone();

    let mut surface =
        ImageSurface::create(Format::ARgb32, db_width as i32, db_height as i32).unwrap();
    let (mut fallback_layer, mut fallback_keys) = build_fallback_widgets();
    let mut needs_complete_redraw = true;
    let mut de_connected = false;
    let mut last_de_check = Instant::now();

    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();

    let epoll = Epoll::new(EpollCreateFlags::empty()).unwrap();
    epoll
        .add(input_tb.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 0))
        .unwrap();
    epoll
        .add(input_main.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 1))
        .unwrap();

    let event_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK) };
    if event_fd < 0 {
        eprintln!("ktouchbar-system: failed to create eventfd");
        return;
    }
    let ef_dup = unsafe { libc::dup(event_fd) };
    if ef_dup < 0 {
        unsafe { libc::close(event_fd); }
        eprintln!("ktouchbar-system: failed to dup eventfd");
        return;
    }
    let _ef_owned = unsafe { OwnedFd::from_raw_fd(ef_dup) };
    epoll.add(
        _ef_owned.as_fd(),
        EpollEvent::new(EpollFlags::EPOLLIN, 2),
    ).unwrap();

    let mut epoll_events = [
        EpollEvent::new(EpollFlags::empty(), 0),
        EpollEvent::new(EpollFlags::empty(), 0),
        EpollEvent::new(EpollFlags::empty(), 0),
        EpollEvent::new(EpollFlags::empty(), 0),
    ];

    let mut digitizer: Option<InputDevice> = None;
    let mut touches: HashMap<u32, usize> = HashMap::new();

    if let Err(e) = dbus_conn.object_server().at(
        "/org/ktouchbar/Hardware",
        HardwareService {
            state: hw_state.clone(),
            backlight: backlight.clone(),
            uinput: uinput_shared,
            event_fd,
        },
    ) {
        eprintln!("ktouchbar-system: failed to register D-Bus object: {e}");
        return;
    }
    if let Err(e) = dbus_conn.request_name("org.ktouchbar.Hardware") {
        eprintln!("ktouchbar-system: failed to request D-Bus name: {e}");
        return;
    }

    println!("ktouchbar-system: D-Bus service ready on system bus");

    loop {
        if last_de_check.elapsed() >= DE_CHECK_INTERVAL {
            last_de_check = Instant::now();
            let was_connected = de_connected;
            de_connected = check_name_has_owner(&dbus_conn, "org.ktouchbar.UserInterface");
            if de_connected != was_connected {
                if de_connected {
                    println!("ktouchbar-system: DE connected");
                } else {
                    println!("ktouchbar-system: DE disconnected, fallback rendering");
                    let (fb_layer, fb_keys) = build_fallback_widgets();
                    fallback_layer = fb_layer;
                    fallback_keys = fb_keys;
                }
                needs_complete_redraw = true;
            }
        }

        if de_connected {
            let mut hw = hw_state.lock().unwrap();
            if hw.pending_frame {
                hw.pending_frame = false;
                let mut map = drm.map().unwrap();
                let dst = map.as_mut();
                let len = min(dst.len(), hw.frame_pixels.len());
                dst[..len].copy_from_slice(&hw.frame_pixels[..len]);
                drop(map);
                let _ = drm.dirty(&[ClipRect::new(0, 0, height, width)]);
            }
            drop(hw);
        } else {
            let clips = fallback_layer.draw(
                &cfg,
                width as i32,
                height as i32,
                &surface,
                (0.0, 0.0),
                needs_complete_redraw,
            );
            if needs_complete_redraw || !clips.is_empty() {
                let data = surface.data().unwrap();
                let mut map = drm.map().unwrap();
                map.as_mut()[..data.len()].copy_from_slice(&data);
                drop(map);
                let _ = drm.dirty(&clips);
                needs_complete_redraw = false;
            }
        }

        let n_events = match epoll.wait(&mut epoll_events, Option::<u16>::None) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("ktouchbar-system: epoll wait error: {e}");
                0
            }
        };

        for ev in epoll_events[..n_events as usize].iter() {
            if ev.data() == 2 {
                let mut val: u64 = 0;
                unsafe { libc::read(event_fd, &mut val as *mut _ as *mut libc::c_void, 8); }
                last_de_check = Instant::now() - DE_CHECK_INTERVAL;
                if de_connected {
                    let mut hw = hw_state.lock().unwrap();
                    if hw.pending_frame {
                        hw.pending_frame = false;
                        let mut map = drm.map().unwrap();
                        let dst = map.as_mut();
                        let len = min(dst.len(), hw.frame_pixels.len());
                        dst[..len].copy_from_slice(&hw.frame_pixels[..len]);
                        drop(map);
                        let _ = drm.dirty(&[ClipRect::new(0, 0, height, width)]);
                    }
                }
            }
        }

        input_tb.dispatch().unwrap();
        input_main.dispatch().unwrap();
        for event in (&mut input_tb).chain(&mut input_main) {
            let mut bl = bl_main.lock().unwrap();
            bl.process_event(&event);
            let bl_off = bl.current_bl() == 0;
            match event {
                Event::Device(DeviceEvent::Added(evt)) => {
                    let dev = evt.device();
                    if dev.name().contains(" Touch Bar") {
                        digitizer = Some(dev);
                    }
                }
                Event::Keyboard(KeyboardEvent::Key(key))
                    if key.key() == Key::Fn as u32 => {
                        let pressed = matches!(key.key_state(), KeyState::Pressed);
                        hw_state.lock().unwrap().fn_pressed = pressed;
                    }
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer || bl_off {
                        continue;
                    }
                    if de_connected {
                        match te {
                            TouchEvent::Down(dn) => {
                                let x = dn.x_transformed(width as u32);
                                let y = dn.y_transformed(height as u32);
                                let slot = dn.seat_slot();
                                let mut s = hw_state.lock().unwrap();
                                s.pending_touches.push(TouchEventRecord {
                                    slot,
                                    x,
                                    y,
                                    event_type: 0,
                                });
                                touches.insert(slot, 0);
                            }
                            TouchEvent::Motion(mtn) => {
                                let slot = mtn.seat_slot();
                                if touches.contains_key(&slot) {
                                    let x = mtn.x_transformed(width as u32);
                                    let y = mtn.y_transformed(height as u32);
                                    hw_state.lock().unwrap().pending_touches.push(TouchEventRecord {
                                        slot,
                                        x,
                                        y,
                                        event_type: 1,
                                    });
                                }
                            }
                            TouchEvent::Up(up) => {
                                let slot = up.seat_slot();
                                if touches.remove(&slot).is_some() {
                                    hw_state.lock().unwrap().pending_touches.push(TouchEventRecord {
                                        slot,
                                        x: 0.0,
                                        y: 0.0,
                                        event_type: 2,
                                    });
                                }
                            }
                            _ => {}
                        }
                    } else {
                        match te {
                            TouchEvent::Down(dn) => {
                                let x = dn.x_transformed(width as u32);
                                let y = dn.y_transformed(height as u32);
                                if let Some((w_idx, _child_idx)) = fallback_layer.hit(width, height, x, y, None) {
                                    touches.insert(dn.seat_slot(), w_idx);
                                    if let Some(key) = fallback_keys.get(w_idx).copied() {
                                        toggle_key(&uinput_fb.lock().unwrap(), key, 1);
                                    }
                                }
                            }
                            TouchEvent::Up(up) => {
                                let slot = up.seat_slot();
                                if let Some(w_idx) = touches.remove(&slot) {
                                    if let Some(key) = fallback_keys.get(w_idx).copied() {
                                        toggle_key(&uinput_fb.lock().unwrap(), key, 0);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        bl_main.lock().unwrap().update_backlight();
    }
}

fn check_name_has_owner(conn: &zbus::blocking::Connection, name: &str) -> bool {
    let result: zbus::Result<bool> = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "NameHasOwner",
            &(name,),
        )
        .and_then(|msg| msg.body());
    result.unwrap_or(false)
}

fn run_fallback_only(drm: &mut DrmBackend, width: u16, height: u16, db_width: u32, db_height: u32) {
    let (mut layer, _) = build_fallback_widgets();
    let mut surface =
        ImageSurface::create(Format::ARgb32, db_width as i32, db_height as i32).unwrap();
    let cfg = Config::fallback_defaults();

    let _ = layer.draw(&cfg, width as i32, height as i32, &surface, (0.0, 0.0), true);
    let data = surface.data().unwrap();
    let mut map = drm.map().unwrap();
    map.as_mut()[..data.len()].copy_from_slice(&data);
    drop(map);
    let _ = drm.dirty(&[ClipRect::new(0, 0, height, width)]);

    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGTERM);
    sigset.wait().unwrap();
}

fn build_fallback_widgets() -> (FunctionLayer, Vec<Key>) {
    use ktouchbar::config::{WidgetConfig, CommonFields};
    use ktouchbar::widget::{Action, WidgetConfigCtx};
    let mut widgets_configs: Vec<WidgetConfig> = Vec::new();
    let mut key_map: Vec<Key> = Vec::new();
    let mut add_key = |text: String, key: Key| {
        widgets_configs.push(WidgetConfig::Button {
            icon: None,
            text: Some(text),
            action: Some(Action::Key(vec![key])),
            common: CommonFields {
                show_outline: Some(false),
                margin: Some(10),
                ..Default::default()
            },
        });
        key_map.push(key);
    };

    for i in 1..=12 {
        add_key(
            format!("F{i}"),
            match i {
                1 => Key::F1,
                2 => Key::F2,
                3 => Key::F3,
                4 => Key::F4,
                5 => Key::F5,
                6 => Key::F6,
                7 => Key::F7,
                8 => Key::F8,
                9 => Key::F9,
                10 => Key::F10,
                11 => Key::F11,
                _ => Key::F12,
            },
        );
    }
    (FunctionLayer::with_config(widgets_configs, &WidgetConfigCtx { global_theme: None, default_padding: None, slider_color_source: None, global_outline_color: None, global_active_color: None, global_slider_live_update: None, global_track_outline: None }), key_map)
}
