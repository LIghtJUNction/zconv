//! Event encoding with persistent state and buffers borrowed only for each call.
use super::*;

/// Storage retained by the caller between calls to [`EventEncoderState::encode`].
/// Growing these slices must preserve their contents. Initialize new item slots
/// with [`ActiveItem::EMPTY`]. Checkpoints and tokens are scratch storage.
pub struct EventEncoderBuffers<'a> {
    pub tokens: &'a mut [Token],
    pub items: &'a mut [ActiveItem],
    pub checkpoints: &'a mut [ActiveItem],
    pub aggregate: &'a mut [u8],
}

/// Allocation-free stream encoder state without borrowed storage.
///
/// Callers can own this state alongside growable buffers without a
/// self-referential struct. Protocol and policy are fixed at construction;
/// response identity context is supplied on each call and must stay consistent.
/// Successful bytes can be published immediately. On error, discard output;
/// capacity errors roll back state and can retry the same event batch.
pub struct EventEncoderState {
    config: StreamConfig<'static>,
    state: State,
}

impl EventEncoderState {
    pub fn new(config: StreamConfig<'_>) -> Result<Self, Error> {
        validate_stream_format(config.source)?;
        validate_stream_format(config.target)?;
        let state = State::new(config.context)?;
        Ok(Self {
            config: StreamConfig {
                source: config.source,
                target: config.target,
                strategy: Strategy::Ir,
                json_limits: config.json_limits,
                context: StreamContext::default(),
                policy: config.policy,
                validate_snapshots: config.validate_snapshots,
                allow_partial_usage: config.allow_partial_usage,
            },
            state,
        })
    }

    pub fn usage(&self) -> Usage {
        self.state.usage.usage()
    }

    pub fn report(&self) -> Report {
        self.state.report
    }

    pub fn is_finished(&self) -> bool {
        self.state.ended
    }

    pub fn encode(
        &mut self,
        context: StreamContext<'_>,
        payload: &[u8],
        events: &[Event],
        buffers: EventEncoderBuffers<'_>,
        output: &mut [u8],
    ) -> Result<usize, Status> {
        stage(
            StreamConfig {
                context,
                ..self.config
            },
            payload,
            events,
            buffers,
            output,
            &mut self.state,
        )
    }

    /// Encode a batch without committing progress or persistent buffer changes.
    ///
    /// Discard output on error, just as with [`Self::encode`]. Successful bytes
    /// can be held until an application commits an external operation. The
    /// encoder remains at its previous position, so a rejected operation can
    /// instead encode a failure. Do not publish both preview and replacement
    /// bytes. To continue using this encoder after publishing a preview, first
    /// commit the same batch with `encode` and discard that duplicate output.
    pub fn preview(
        &mut self,
        context: StreamContext<'_>,
        payload: &[u8],
        events: &[Event],
        buffers: EventEncoderBuffers<'_>,
        output: &mut [u8],
    ) -> Result<usize, Status> {
        let saved = self.state;
        let EventEncoderBuffers {
            tokens,
            items,
            checkpoints,
            aggregate,
        } = buffers;
        let result = self.encode(
            context,
            payload,
            events,
            EventEncoderBuffers {
                tokens,
                items: &mut *items,
                checkpoints: &mut *checkpoints,
                aggregate: &mut *aggregate,
            },
            output,
        );
        if result.is_ok() {
            restore(saved, &mut self.state, items, checkpoints, aggregate);
        }
        result
    }
}

fn restore(
    saved: State,
    state: &mut State,
    items: &mut [ActiveItem],
    checkpoints: &[ActiveItem],
    aggregate: &mut [u8],
) {
    for item in &checkpoints[..saved.item_count] {
        if item.tail != NONE {
            write_usize(aggregate, item.tail, NONE);
        }
    }
    items.copy_from_slice(&checkpoints[..items.len()]);
    *state = saved;
}

pub(super) fn stage(
    config: StreamConfig<'_>,
    payload: &[u8],
    events: &[Event],
    buffers: EventEncoderBuffers<'_>,
    output: &mut [u8],
    state: &mut State,
) -> Result<usize, Status> {
    if state.ended {
        return Err(Status::Failed(Error::AlreadyFinished));
    }
    let EventEncoderBuffers {
        tokens,
        items,
        checkpoints,
        aggregate,
    } = buffers;
    if items.len() < state.item_count || checkpoints.len() < items.len() {
        return Err(Status::NeedBuffer {
            buffer: BufferKind::Items,
            needed: state.item_count.max(items.len()),
            exact: false,
        });
    }
    if aggregate.len() < state.aggregate_used {
        return Err(Status::NeedBuffer {
            buffer: BufferKind::Aggregate,
            needed: state.aggregate_used,
            exact: false,
        });
    }
    if state.item_count == 0 {
        items.fill(ActiveItem::EMPTY);
    }
    let saved = *state;
    checkpoints[..items.len()].copy_from_slice(items);
    let result = (|| {
        let mut emitter =
            encode::Emitter::new(config, Writer::new(output), state, items, aggregate);
        for event in events {
            replay(*event, payload, &mut emitter, tokens, config.json_limits)?;
        }
        emitter.state.report.ir_events =
            emitter.state.report.ir_events.saturating_add(events.len());
        let length = emitter.writer.finish()?;
        validate_arguments(config.json_limits, state, items, aggregate, tokens)?;
        Ok(length)
    })();
    match result {
        Ok(length) => Ok(length),
        Err(error) => {
            restore(saved, state, items, checkpoints, aggregate);
            Err(capacity_status(error))
        }
    }
}
