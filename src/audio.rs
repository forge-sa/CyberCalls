//! PipeWire side: notice when the Telegram client opens its call streams, then tap its playback
//! (the peer) and the microphone it records from (you) as two separate 16 kHz mono feeds.

use anyhow::Result;
use crossbeam_channel::Sender;
use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::{Msg, Speaker};

/// One tapped stream; `id` keeps per-stream VAD state apart.
pub struct Chunk {
    pub id: u32,
    pub speaker: Speaker,
    pub samples: Vec<f32>,
}

struct Capture {
    _listener: pw::stream::StreamListener<()>,
    _stream: pw::stream::StreamRc,
}

struct State {
    core: pw::core::CoreRc,
    tx: Sender<Msg>,
    app_node: String,
    /// client playback stream node id -> object.serial
    outputs: HashMap<u32, String>,
    /// client capture stream node id -> its target.object (the mic), if pinned
    inputs: HashMap<u32, Option<String>>,
    remote: HashMap<u32, Capture>,
    mic: Option<Capture>,
    in_call: bool,
}

/// Blocks forever running the PipeWire main loop.
pub fn run(app_node: String, tx: Sender<Msg>) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;

    let state = Rc::new(RefCell::new(State {
        core,
        tx,
        app_node,
        outputs: HashMap::new(),
        inputs: HashMap::new(),
        remote: HashMap::new(),
        mic: None,
        in_call: false,
    }));

    let added = state.clone();
    let removed = state.clone();
    let _listener = registry
        .add_listener_local()
        .global(move |obj| {
            if obj.type_ != ObjectType::Node {
                return;
            }
            let Some(props) = obj.props else { return };
            let mut st = added.borrow_mut();
            if !props.get("node.name").is_some_and(|n| n.contains(st.app_node.as_str())) {
                return;
            }
            let serial = props.get("object.serial").unwrap_or_default().to_owned();
            match props.get("media.class") {
                Some("Stream/Output/Audio") => {
                    log::debug!("client playback stream {} (serial {serial})", obj.id);
                    st.outputs.insert(obj.id, serial);
                }
                Some("Stream/Input/Audio") => {
                    log::debug!("client capture stream {} (serial {serial})", obj.id);
                    st.inputs.insert(obj.id, props.get("target.object").map(str::to_owned));
                }
                _ => return,
            }
            st.sync();
        })
        .global_remove(move |id| {
            let mut st = removed.borrow_mut();
            if st.outputs.remove(&id).is_some() | st.inputs.remove(&id).is_some() {
                st.sync();
            }
        })
        .register();

    mainloop.run();
    Ok(())
}

impl State {
    /// Reconcile our taps with the client's streams. The client only records from the mic
    /// while a call is connected, which makes that stream the "call is live" signal.
    fn sync(&mut self) {
        let in_call = !self.inputs.is_empty();
        if in_call != self.in_call {
            self.in_call = in_call;
            log::info!("call audio {}", if in_call { "up" } else { "down" });
            let _ = self.tx.send(Msg::CallAudio(in_call));
        }
        if !in_call {
            self.remote.clear();
            self.mic = None;
            return;
        }
        if self.mic.is_none() {
            let target = self.inputs.values().flatten().next().cloned();
            self.mic = self.tap(0, Speaker::Me, target);
        }
        let outputs = &self.outputs;
        self.remote.retain(|id, _| outputs.contains_key(id));
        let new: Vec<(u32, String)> = self
            .outputs
            .iter()
            .filter(|(id, _)| !self.remote.contains_key(id))
            .map(|(id, s)| (*id, s.clone()))
            .collect();
        for (id, serial) in new {
            if let Some(c) = self.tap(id, Speaker::Peer, Some(serial)) {
                self.remote.insert(id, c);
            }
        }
    }

    fn tap(&self, id: u32, speaker: Speaker, target: Option<String>) -> Option<Capture> {
        match capture(&self.core, id, speaker, target.clone(), self.tx.clone()) {
            Ok(c) => {
                log::info!("tapping {speaker:?} from {}", target.as_deref().unwrap_or("default source"));
                Some(c)
            }
            Err(e) => {
                log::error!("could not tap {speaker:?}: {e}");
                None
            }
        }
    }
}

fn capture(
    core: &pw::core::CoreRc,
    id: u32,
    speaker: Speaker,
    target: Option<String>,
    tx: Sender<Msg>,
) -> Result<Capture> {
    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::NODE_NAME => format!("cybercalls-{}", if speaker == Speaker::Me { "mic" } else { "peer" }),
        // never get re-routed to the default mic if the client's stream goes away
        *pw::keys::NODE_DONT_RECONNECT => "true",
        "node.passive" => "true",
    };
    if let Some(t) = target {
        props.insert("target.object", t);
    }
    let stream = pw::stream::StreamRc::new(core.clone(), "cybercalls", props)?;

    let listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let Some(data) = buffer.datas_mut().first_mut() else { return };
            let n = data.chunk().size() as usize / 4;
            let Some(bytes) = data.data() else { return };
            let samples = bytes[..n * 4]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            let _ = tx.send(Msg::Audio(Chunk { id, speaker, samples }));
        })
        .register()?;

    // ask PipeWire to convert to what Whisper wants: 16 kHz mono f32
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(crate::vad::SAMPLE_RATE as u32);
    info.set_channels(1);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&bytes).expect("valid pod")];
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;
    Ok(Capture { _listener: listener, _stream: stream })
}
