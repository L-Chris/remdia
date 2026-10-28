use remdia_buffer::Budget;
use remdia_graph::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
struct Node {
    info: NodeInfo,
    left: u32,
    queue: VecDeque<MediaBuffer>,
    drained: bool,
    trace: Arc<Mutex<Vec<String>>>,
}
impl Component for Node {
    fn info(&self) -> &NodeInfo {
        &self.info
    }
    fn submit(&mut self, b: &MediaBuffer) -> anyhow::Result<Submit> {
        if !self.queue.is_empty() {
            return Ok(Submit::WouldBlock);
        }
        assert!(!self.drained);
        self.trace
            .lock()
            .unwrap()
            .push(format!("submit:{}", b.bytes()[0]));
        self.queue.push_back(b.clone());
        Ok(Submit::Accepted)
    }
    fn receive(&mut self) -> anyhow::Result<Receive> {
        if let Some(b) = self.queue.pop_front() {
            return Ok(Receive::Output(b));
        }
        if self.info.input.is_none() {
            if self.left == 0 {
                return Ok(Receive::End);
            }
            let n = self.left;
            self.left -= 1;
            return Ok(Receive::Output(MediaBuffer::new(
                BufferHeader {
                    stream_id: 0,
                    generation: 1,
                    info: FrameInfo::Bytes {
                        format: "test".into(),
                        offset: None,
                    },
                },
                vec![vec![n as u8; 4]],
            )));
        }
        Ok(if self.drained {
            Receive::End
        } else {
            Receive::NeedInput
        })
    }
    fn drain(&mut self) -> anyhow::Result<()> {
        assert!(self.queue.is_empty());
        self.trace.lock().unwrap().push("drain".into());
        self.drained = true;
        Ok(())
    }
    fn reset(&mut self, _g: u64, _t: i64) -> anyhow::Result<()> {
        self.left = 3;
        self.queue.clear();
        self.drained = false;
        Ok(())
    }
}
fn node(
    input: Option<DataKind>,
    output: DataKind,
    trace: Arc<Mutex<Vec<String>>>,
) -> Box<dyn Component> {
    Box::new(Node {
        info: NodeInfo {
            input: input.map(PortCaps::new),
            output: PortCaps::new(output),
            max_output_bytes: 4,
            media: None,
        },
        left: 3,
        queue: VecDeque::new(),
        drained: false,
        trace,
    })
}
#[test]
fn rejects_cycles_mismatched_ports_and_disconnected_inputs() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let cycle = Graph::from_instances(
        vec![
            (
                "a".into(),
                node(Some(DataKind::Bytes), DataKind::Bytes, trace.clone()),
            ),
            (
                "b".into(),
                node(Some(DataKind::Bytes), DataKind::Bytes, trace.clone()),
            ),
        ],
        &[
            EdgeSpec::between("a", "b", None),
            EdgeSpec::between("b", "a", None),
        ],
        Budget::new(64),
        Cancellation::default(),
    );
    assert!(cycle.err().unwrap().to_string().contains("cycle"));
    let mismatch = Graph::from_instances(
        vec![
            ("a".into(), node(None, DataKind::Audio, trace.clone())),
            (
                "b".into(),
                node(Some(DataKind::Video), DataKind::Video, trace.clone()),
            ),
        ],
        &[EdgeSpec::between("a", "b", None)],
        Budget::new(64),
        Cancellation::default(),
    );
    assert!(mismatch.err().unwrap().to_string().contains("incompatible"));
    let disconnected = Graph::from_instances(
        vec![(
            "b".into(),
            node(Some(DataKind::Bytes), DataKind::Bytes, trace),
        )],
        &[],
        Budget::new(64),
        Cancellation::default(),
    );
    assert!(
        disconnected
            .err()
            .unwrap()
            .to_string()
            .contains("unconnected")
    );
}
#[test]
fn bounded_edges_do_not_lose_inputs_and_drain_after_the_last_buffer() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let budget = Budget::new(64);
    let mut edge = EdgeSpec::between("source", "filter", None);
    edge.max_bytes = 4;
    edge.max_buffers = 1;
    let mut graph = Graph::from_instances(
        vec![
            ("source".into(), node(None, DataKind::Bytes, trace.clone())),
            (
                "filter".into(),
                node(Some(DataKind::Bytes), DataKind::Bytes, trace.clone()),
            ),
        ],
        &[edge],
        budget.clone(),
        Cancellation::default(),
    )
    .unwrap();
    let mut values = Vec::new();
    let mut held = None;
    for _ in 0..100 {
        match graph.poll().unwrap() {
            GraphEvent::Output { buffer, .. } => {
                values.push(buffer.bytes()[0]);
                if held.is_none() {
                    held = Some(buffer);
                }
            }
            GraphEvent::End => break,
            _ => {}
        }
    }
    assert_eq!(values, vec![3, 2, 1]);
    assert_eq!(
        *trace.lock().unwrap(),
        vec!["submit:3", "submit:2", "submit:1", "drain"]
    );
    drop(graph);
    assert!(budget.used() > 0);
    drop(held);
    assert_eq!(budget.used(), 0);
}
