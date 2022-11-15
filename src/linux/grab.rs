// This code is awful. Good luck
use crate::{
    key_from_scancode, linux_keycode_from_key, Event, EventType, GrabError, Key as RdevKey,
};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};
use std::os::unix::net::UnixDatagram;
use std::os::unix::prelude::AsRawFd;
use std::path::Path;
use std::ptr;
use std::thread;
use std::time::Duration;
use std::{
    mem::zeroed,
    os::raw::c_int,
    sync::{mpsc::Sender, Arc, Mutex},
    time::SystemTime,
};
use x11::xlib::{self, Display, GrabModeAsync, KeyPressMask, KeyReleaseMask};

#[derive(Debug)]
pub struct MyDisplay(*mut xlib::Display);
unsafe impl Sync for MyDisplay {}
unsafe impl Send for MyDisplay {}

lazy_static::lazy_static! {
    pub static ref SENDER: Arc<Mutex<Option<Sender<GrabEvent>>>> = Arc::new(Mutex::new(None));
}

const KEYPRESS_EVENT: i32 = 2;

pub static mut GLOBAL_CALLBACK: Option<Box<dyn FnMut(Event) -> Option<Event>>> = None;
pub static FILE_PATH: &str = "/tmp/rdev_service.sock";
const GRAB_KEY: Token = Token(0);
const SERVICE_CLIENT: Token = Token(1);

pub enum GrabEvent {
    Grab,
    UnGrab,
    Exit,
    KeyEvent(Event),
}

fn convert_event(key: RdevKey, is_press: bool) -> Event {
    Event {
        event_type: if is_press {
            EventType::KeyPress(key)
        } else {
            EventType::KeyRelease(key)
        },
        time: SystemTime::now(),
        name: None,
        code: linux_keycode_from_key(key).unwrap_or_default() as _,
        scan_code: linux_keycode_from_key(key).unwrap_or_default() as _,
    }
}

fn grab_keys(display: *mut Display, grab_window: u64) {
    unsafe {
        xlib::XGrabKeyboard(
            display,
            grab_window,
            c_int::from(true),
            GrabModeAsync,
            GrabModeAsync,
            xlib::CurrentTime,
        );
        xlib::XFlush(display);
        thread::sleep(Duration::from_millis(50));
    }
}

fn ungrab_keys(display: *mut Display) {
    unsafe {
        xlib::XUngrabKeyboard(display, xlib::CurrentTime);
        xlib::XFlush(display);
        thread::sleep(Duration::from_millis(50));
    }
}

pub fn enable_grab() -> Result<(), GrabError> {
    if let Some(tx) = &*SENDER.lock().unwrap() {
        tx.send(GrabEvent::Grab).ok();
        /* if too fast: poll cannot perceive events */
        thread::sleep(Duration::from_millis(50));
    } else {
        return Err(GrabError::ListenError);
    };
    Ok(())
}

pub fn disable_grab() -> Result<(), GrabError> {
    if let Some(tx) = &*SENDER.lock().unwrap() {
        tx.send(GrabEvent::UnGrab).ok();
        thread::sleep(Duration::from_millis(50));
    } else {
        return Err(GrabError::ListenError);
    };
    Ok(())
}

pub fn start_grab_listen<T>(callback: T)
where
    T: FnMut(Event) -> Option<Event> + 'static,
{
    unsafe {
        GLOBAL_CALLBACK = Some(Box::new(callback));
    }
    start_grab_service();
    thread::sleep(Duration::from_millis(50));
}

pub fn exit_grab_listen() -> Result<(), GrabError> {
    if let Some(tx) = &*SENDER.lock().unwrap() {
        if tx.send(GrabEvent::Exit).is_err() {
            return Err(GrabError::ListenError);
        };
    } else {
        return Err(GrabError::ListenError);
    };
    Ok(())
}

fn send_to_client(grab: bool) {
    let socket = UnixDatagram::unbound().unwrap();
    if socket.connect(FILE_PATH).is_ok() {
        let message = if grab { b"1" } else { b"0" };
        socket
            .send(message)
            .expect("[-] grab error in rdev: recv function failed");
    };
}

fn start_grab_service() {
    let (send, recv) = std::sync::mpsc::channel::<GrabEvent>();
    *SENDER.lock().unwrap() = Some(send);
    start_grab_thread();

    thread::spawn(move || loop {
        if let Ok(data) = recv.recv() {
            match data {
                GrabEvent::Grab => {
                    send_to_client(true);
                }
                GrabEvent::UnGrab => {
                    send_to_client(false);
                }
                GrabEvent::KeyEvent(event) => unsafe {
                    if let Some(callback) = &mut GLOBAL_CALLBACK {
                        callback(event);
                    }
                },
                GrabEvent::Exit => {
                    break;
                }
            }
        }
    });
}

fn unlink_socket(path: impl AsRef<Path>) {
    let path = path.as_ref();
    if Path::new(path).exists() {
        let result = std::fs::remove_file(path);
        if let Err(e) = result {
            println!("[-] grab error in rdev: Couldn't remove the file: {:?}", e);
        }
    }
}

fn start_grab_thread() {
    thread::spawn(move || {
        // init the thread
        let display = unsafe { xlib::XOpenDisplay(ptr::null()) };
        /* todo! how to get  Exception*/
        if display.is_null() {
            panic!("[-] grab error in rdev: MissingDisplayError");
        }
        let screen_number = unsafe { xlib::XDefaultScreen(display) };
        /* todo! Multiple Display */
        let screen = unsafe { xlib::XScreenOfDisplay(display, screen_number) };
        if screen.is_null() {
            panic!("[-] grab error in rdev: XScreenOfDisplay Error");
        }
        let grab_window = unsafe { xlib::XRootWindowOfScreen(screen) };

        unsafe {
            xlib::XSelectInput(display, grab_window, KeyPressMask | KeyReleaseMask);
        }

        let grab_fd = unsafe { xlib::XConnectionNumber(display) };
        unlink_socket(FILE_PATH);
        let socket = match UnixDatagram::bind(FILE_PATH) {
            Ok(socket) => socket,
            Err(err) => panic!("[-] grab error in rdev: {:?}", err),
        };
        if socket.set_nonblocking(true).is_err() {
            panic!("[-] grab error in rdev: set_nonblocking");
        };

        if let Ok(mut poll) = Poll::new() {
            let mut events = Events::with_capacity(128);
            if poll
                .registry()
                .register(&mut SourceFd(&grab_fd), GRAB_KEY, Interest::READABLE)
                .is_err()
            {
                panic!("[-] grab error in rdev: Poll register grab fd failed");
            };
            if poll
                .registry()
                .register(
                    &mut SourceFd(&socket.as_raw_fd()),
                    SERVICE_CLIENT,
                    Interest::READABLE,
                )
                .is_err()
            {
                panic!("[-] grab error in rdev: Poll register socket fd failed");
            };

            let mut x_event: xlib::XEvent = unsafe { zeroed() };

            loop {
                if poll.poll(&mut events, None).is_err() {
                    println!("[-] grab error in rdev: Poll poll failed");
                };
                for event in &events {
                    match event.token() {
                        SERVICE_CLIENT => {
                            let mut buf = vec![0; 1];
                            socket
                                .recv(buf.as_mut_slice())
                                .expect("recv function failed");
                            // if recv "1": grab key
                            if buf[0] == 49 {
                                grab_keys(display, grab_window);
                            } else {
                                ungrab_keys(display);
                            }
                        }
                        GRAB_KEY => unsafe {
                            while xlib::XPending(display) > 0 {
                                xlib::XNextEvent(display, &mut x_event);
                                let key = key_from_scancode(x_event.key.keycode);
                                let is_press = x_event.type_ == KEYPRESS_EVENT;
                                let event = convert_event(key, is_press);
                                if let Some(tx) = &*SENDER.lock().unwrap() {
                                    tx.send(GrabEvent::KeyEvent(event)).ok();
                                }
                            }
                        },
                        _ => {}
                    }
                }
            }
        } else {
            panic!("[-] grab error in rdev: Create Poll failed");
        }
    });
}
