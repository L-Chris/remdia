use crate::*;
use anyhow::{Context, Result, ensure};
use remdia_buffer::Budget;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSpec {
    pub id: String,
    #[serde(flatten)]
    pub config: NodeConfig,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EdgeSpec {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub stream_id: Option<u32>,
    #[serde(default = "default_bytes")]
    pub max_bytes: usize,
    #[serde(default = "default_count")]
    pub max_buffers: usize,
    #[serde(default = "default_time")]
    pub max_duration_us: u64,
}
fn default_bytes() -> usize {
    32 * 1024 * 1024
}
fn default_count() -> usize {
    8
}
fn default_time() -> u64 {
    2_000_000
}
impl EdgeSpec {
    pub fn between(from: impl Into<String>, to: impl Into<String>, stream_id: Option<u32>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
            stream_id,
            max_bytes: default_bytes(),
            max_buffers: default_count(),
            max_duration_us: default_time(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GraphSpec {
    pub nodes: Vec<NodeSpec>,
    pub edges: Vec<EdgeSpec>,
}
// Polling must not allocate an additional box per output buffer.
#[allow(clippy::large_enum_variant)]
pub enum GraphEvent {
    Output { node: String, buffer: MediaBuffer },
    Pending,
    End,
}
struct Node {
    id: String,
    component: Box<dyn Component>,
    incoming: Vec<usize>,
    outgoing: Vec<usize>,
    pending: Option<MediaBuffer>,
    ended: bool,
    draining: bool,
}
struct Edge {
    spec: EdgeSpec,
    from: usize,
    to: usize,
    queue: VecDeque<MediaBuffer>,
    bytes: usize,
    duration: u64,
}
pub struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    order: Vec<usize>,
    budget: Budget,
    cancel: Cancellation,
    generation: u64,
}
impl Graph {
    pub fn build(
        spec: &GraphSpec,
        factory: &dyn Factory,
        budget: Budget,
        cancel: Cancellation,
    ) -> Result<Self> {
        let mut instances = Vec::new();
        for n in &spec.nodes {
            instances.push((
                n.id.clone(),
                factory
                    .create(&n.config, cancel.clone())
                    .with_context(|| format!("create node {}", n.id))?,
            ));
        }
        Self::from_instances(instances, &spec.edges, budget, cancel)
    }
    pub fn from_instances(
        instances: Vec<(String, Box<dyn Component>)>,
        connections: &[EdgeSpec],
        budget: Budget,
        cancel: Cancellation,
    ) -> Result<Self> {
        ensure!(!instances.is_empty(), "empty media graph");
        let mut ids = BTreeMap::new();
        let mut nodes = Vec::new();
        for (id, component) in instances {
            ensure!(
                ids.insert(id.clone(), nodes.len()).is_none(),
                "duplicate graph node {id}"
            );
            ensure!(
                component.info().max_output_bytes <= budget.limit(),
                "node {id} minimum output reservation exceeds budget"
            );
            nodes.push(Node {
                id,
                component,
                incoming: Vec::new(),
                outgoing: Vec::new(),
                pending: None,
                ended: false,
                draining: false,
            });
        }
        let mut edges = Vec::new();
        for spec in connections {
            let from = *ids.get(&spec.from).context("unknown edge source")?;
            let to = *ids.get(&spec.to).context("unknown edge destination")?;
            ensure!(from != to, "self edge");
            ensure!(
                spec.max_bytes > 0 && spec.max_buffers > 0 && spec.max_duration_us > 0,
                "invalid edge budget"
            );
            let input = nodes[to]
                .component
                .info()
                .input
                .as_ref()
                .context("cannot connect an input to a source")?;
            ensure!(
                input.accepts(&nodes[from].component.info().output),
                "incompatible ports: {} -> {}",
                spec.from,
                spec.to
            );
            ensure!(
                !edges.iter().any(|e: &Edge| e.from == from
                    && e.to == to
                    && e.spec.stream_id == spec.stream_id),
                "duplicate graph edge"
            );
            nodes[from].outgoing.push(edges.len());
            nodes[to].incoming.push(edges.len());
            edges.push(Edge {
                spec: spec.clone(),
                from,
                to,
                queue: VecDeque::new(),
                bytes: 0,
                duration: 0,
            });
        }
        let mut degree: Vec<_> = nodes.iter().map(|n| n.incoming.len()).collect();
        let mut ready: VecDeque<_> = degree
            .iter()
            .enumerate()
            .filter(|(_, n)| **n == 0)
            .map(|(i, _)| i)
            .collect();
        let mut order = Vec::new();
        while let Some(i) = ready.pop_front() {
            order.push(i);
            for e in &nodes[i].outgoing {
                let to = edges[*e].to;
                degree[to] -= 1;
                if degree[to] == 0 {
                    ready.push_back(to);
                }
            }
        }
        ensure!(order.len() == nodes.len(), "media graph contains a cycle");
        for n in &nodes {
            ensure!(
                n.component.info().input.is_none() || !n.incoming.is_empty(),
                "unconnected input on {}",
                n.id
            );
        }
        Ok(Self {
            nodes,
            edges,
            order,
            budget,
            cancel,
            generation: 1,
        })
    }
    pub fn budget(&self) -> &Budget {
        &self.budget
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn node_info(&self, id: &str) -> Option<&NodeInfo> {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.component.info())
    }
    pub fn command(&mut self, id: &str, value: &Value) -> Result<Option<MediaBuffer>> {
        self.cancel.begin();
        self.nodes
            .iter_mut()
            .find(|n| n.id == id)
            .context("unknown command node")?
            .component
            .command(value)
    }
    pub fn reset(&mut self, target_us: i64) -> Result<u64> {
        ensure!(target_us >= 0, "negative seek");
        self.cancel.begin();
        let generation = self
            .generation
            .checked_add(1)
            .context("generation exhausted")?;
        for &i in &self.order {
            self.nodes[i].component.reset(generation, target_us)?;
        }
        for n in &mut self.nodes {
            n.pending = None;
            n.ended = false;
            n.draining = false;
        }
        for e in &mut self.edges {
            e.queue.clear();
            e.bytes = 0;
            e.duration = 0;
        }
        self.generation = generation;
        Ok(generation)
    }
    /// One bounded scheduling pass. External consumers decide when to pull again.
    pub fn poll(&mut self) -> Result<GraphEvent> {
        ensure!(!self.cancel.is_cancelled(), "cancelled");
        let mut progress = false;
        let mut denied = false;
        let ended: Vec<_> = self.nodes.iter().map(|n| n.ended).collect();
        for &i in self.order.iter().rev() {
            let n = &mut self.nodes[i];
            if let Some(buffer) = n.pending.take() {
                if n.outgoing.is_empty() {
                    return Ok(GraphEvent::Output {
                        node: n.id.clone(),
                        buffer,
                    });
                }
                let targets: Vec<_> = n
                    .outgoing
                    .iter()
                    .copied()
                    .filter(|e| {
                        self.edges[*e]
                            .spec
                            .stream_id
                            .is_none_or(|s| s == buffer.header.stream_id)
                    })
                    .collect();
                let mut blocked = false;
                for &e in &targets {
                    let edge = &self.edges[e];
                    ensure!(
                        buffer.len() <= edge.spec.max_bytes,
                        "single buffer exceeds edge {} -> {} byte budget",
                        edge.spec.from,
                        edge.spec.to
                    );
                    if edge.queue.len() >= edge.spec.max_buffers
                        || buffer.len() > edge.spec.max_bytes.saturating_sub(edge.bytes)
                        || (!edge.queue.is_empty()
                            && buffer.header.duration_us()
                                > edge.spec.max_duration_us.saturating_sub(edge.duration))
                    {
                        blocked = true;
                    }
                }
                if blocked {
                    n.pending = Some(buffer);
                    continue;
                }
                for e in targets {
                    let edge = &mut self.edges[e];
                    edge.bytes += buffer.len();
                    edge.duration = edge.duration.saturating_add(buffer.header.duration_us());
                    edge.queue.push_back(buffer.clone());
                }
                progress = true;
            }
            if n.ended {
                continue;
            }
            let maximum = n.component.info().max_output_bytes;
            let reservation = match self.budget.reserve(maximum) {
                Ok(v) => v,
                Err(_) => {
                    denied = true;
                    continue;
                }
            };
            match n
                .component
                .receive()
                .with_context(|| format!("receive from {}", n.id))?
            {
                Receive::Output(mut buffer) => {
                    ensure!(
                        buffer.len() <= maximum,
                        "component {} exceeded output reservation",
                        n.id
                    );
                    buffer.validate()?;
                    buffer.header.generation = self.generation;
                    let mut reservation = reservation;
                    reservation.shrink(buffer.len());
                    n.pending = Some(buffer.with_budget(reservation));
                    progress = true;
                    continue;
                }
                Receive::End => {
                    n.ended = true;
                    progress = true;
                    continue;
                }
                Receive::NeedInput => {}
            }
            drop(reservation);
            let mut submitted = false;
            for &e in &n.incoming {
                let edge = &mut self.edges[e];
                if let Some(buffer) = edge.queue.front() {
                    match n
                        .component
                        .submit(buffer)
                        .with_context(|| format!("submit to {}", n.id))?
                    {
                        Submit::Accepted => {
                            let buffer = edge.queue.pop_front().unwrap();
                            edge.bytes -= buffer.len();
                            edge.duration =
                                edge.duration.saturating_sub(buffer.header.duration_us());
                            progress = true;
                            submitted = true;
                            break;
                        }
                        Submit::WouldBlock => {}
                    }
                }
            }
            if !submitted && !n.draining && !n.incoming.is_empty() {
                // Parent EOF only propagates after every queued input was accepted.
                let parents_done = n
                    .incoming
                    .iter()
                    .all(|e| self.edges[*e].queue.is_empty() && ended[self.edges[*e].from]);
                if parents_done {
                    self.nodes[i].component.drain()?;
                    self.nodes[i].draining = true;
                    progress = true;
                }
            }
        }
        if self.nodes.iter().all(|n| n.ended && n.pending.is_none()) {
            return Ok(GraphEvent::End);
        }
        ensure!(
            progress || !denied,
            "managed budget exhausted; release retained frames before polling again"
        );
        Ok(GraphEvent::Pending)
    }
}
