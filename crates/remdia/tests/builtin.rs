use anyhow::Result;
use remdia::graph::{
    Component, ComponentDescriptor, Control, DataKind, NodeInfo, PortCaps, Receive,
    ffi::BuiltinPlugin,
};
use remdia::registry::Registry;
use remdia::{
    BufferHeader, Cancellation, Engine, FrameInfo, GraphEvent, GraphSpec, MediaBuffer, NodeConfig,
    NodeSpec,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
struct Source {
    info: NodeInfo,
    control: Control,
    panic: bool,
    sent: bool,
}
impl Component for Source {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn receive(&mut self) -> Result<Receive> {
        self.control.check()?;
        assert!(!self.panic, "synthetic component failure");
        if self.sent {
            return Ok(Receive::End);
        }
        self.sent = true;
        let bytes = vec![1, 2, 3, 4];
        ORIGINAL.store(bytes.as_ptr() as usize, Ordering::Relaxed);
        Ok(Receive::Output(MediaBuffer::new(
            BufferHeader {
                stream_id: 0,
                generation: 1,
                info: FrameInfo::Bytes {
                    format: "test".into(),
                    offset: None,
                },
            },
            vec![bytes],
        )))
    }
    fn drain(&mut self) -> Result<()> {
        Ok(())
    }
    fn reset(&mut self, _: u64, _: i64) -> Result<()> {
        self.sent = false;
        Ok(())
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        let _ = self.control.check();
    } // context must still be live
}
fn describe() -> Vec<ComponentDescriptor> {
    vec![ComponentDescriptor {
        id: "test.source".into(),
        role: "source".into(),
        input: None,
        output: PortCaps::new(DataKind::Bytes),
    }]
}
fn create(config: &NodeConfig, control: Control) -> Result<Box<dyn Component>> {
    Ok(Box::new(Source {
        info: NodeInfo {
            input: None,
            output: PortCaps::new(DataKind::Bytes),
            max_output_bytes: 4,
            media: None,
        },
        control,
        panic: config.settings["panic"] == true,
        sent: false,
    }))
}
fn graph(panic: bool) -> (Engine, remdia::Graph, Cancellation) {
    let mut registry = Registry::default();
    registry
        .register("test", BuiltinPlugin { describe, create })
        .unwrap();
    assert!(
        registry
            .register("test", BuiltinPlugin { describe, create })
            .is_err()
    );
    let engine = Engine::from_registry(registry, 64);
    let spec = GraphSpec {
        nodes: vec![NodeSpec {
            id: "source".into(),
            config: NodeConfig {
                component: "test.source".into(),
                settings: json!({"panic":panic}),
            },
        }],
        edges: vec![],
    };
    let cancel = Cancellation::default();
    let graph = unsafe { engine.graph(&spec, cancel.clone()).unwrap() };
    (engine, graph, cancel)
}
#[test]
fn native_buffers_and_cancellation_survive_engine_close() {
    let (engine, mut graph, cancel) = graph(false);
    let budget = engine.budget().clone();
    let buffer = (0..10)
        .find_map(|_| match graph.poll().unwrap() {
            GraphEvent::Output { buffer, .. } => Some(buffer),
            GraphEvent::Pending => None,
            GraphEvent::End => panic!("missing output"),
        })
        .expect("source stalled");
    assert_eq!(
        buffer.bytes().as_ptr() as usize,
        ORIGINAL.load(Ordering::Relaxed)
    );
    assert_eq!(budget.used(), 4);
    drop(engine);
    cancel.cancel();
    assert!(graph.poll().is_err());
    drop(graph);
    assert_eq!(buffer.bytes(), &[1, 2, 3, 4]);
    drop(buffer);
    assert_eq!(budget.used(), 0);
}
#[test]
fn builtin_panics_are_contained_and_poison_the_node() {
    let (_, mut graph, _) = graph(true);
    assert!(format!("{:#}", graph.poll().err().unwrap()).contains("panic contained"));
    assert!(format!("{:#}", graph.poll().err().unwrap()).contains("poisoned"));
}
