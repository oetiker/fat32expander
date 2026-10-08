//! What a resize reports while it runs ([F-08]).
//!
//! The library never prints. It hands each event to a callback the caller
//! supplies, and the caller decides what reaches a terminal or a log, and on
//! which stream. `resize_fat32` passes a callback that drops everything.

/// Something a resize reports while it runs.
///
/// [`Step`](ResizeEvent::Step) and [`Resuming`](ResizeEvent::Resuming) and
/// [`StaleCheckpointIgnored`](ResizeEvent::StaleCheckpointIgnored) are always
/// reported. The others are detail, reported only when
/// [`ResizeOptions::verbose`](crate::ResizeOptions::verbose) is set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResizeEvent {
    /// A step of the resize completed. These are the entries of
    /// [`ResizeResult::operations`](crate::ResizeResult::operations), reported
    /// as they happen, so a caller still has them when the resize fails.
    Step(String),
    /// An interrupted resize was found. A real run resumes it; a dry run
    /// previews that resume.
    Resuming {
        /// The phase the interrupted run had reached.
        phase: &'static str,
        dry_run: bool,
    },
    /// A checkpoint was found that belongs to another filesystem; it is
    /// ignored, and a fresh resize overwrites it.
    StaleCheckpointIgnored { sector: u64 },
    /// The layout before and after the resize. Detail.
    Layout {
        old_total_sectors: u32,
        old_fat_size: u32,
        /// `None` if the old layout does not fit in its own volume.
        old_data_clusters: Option<u32>,
        new_total_sectors: u32,
        new_fat_size: u32,
        new_data_clusters: u32,
        fat_needs_growth: bool,
    },
    /// The data shift plan: how many clusters move. Detail.
    ShiftPlanned { clusters: usize },
    /// The data shift starts. Detail.
    ShiftStarting {
        /// Clusters this run moves.
        remaining: usize,
        /// Clusters in the whole plan.
        total: usize,
        old_first_data_sector: u64,
        new_first_data_sector: u64,
        /// On a resume, the cluster the shift continues at.
        resumed_at: Option<u32>,
    },
    /// One cluster was copied to its new sector. Detail.
    ClusterMoved {
        cluster: u32,
        from_sector: u64,
        to_sector: u64,
        /// 1-based position of this move in this run.
        done: usize,
        /// Moves in this run.
        of: usize,
    },
}

/// Delivers events to the caller's callback and keeps the operations log.
pub(crate) struct Reporter<'a> {
    sink: &'a mut dyn FnMut(&ResizeEvent),
    verbose: bool,
    operations: Vec<String>,
}

impl<'a> Reporter<'a> {
    pub(crate) fn new(sink: &'a mut dyn FnMut(&ResizeEvent), verbose: bool) -> Self {
        Self {
            sink,
            verbose,
            operations: Vec::new(),
        }
    }

    /// Record a completed step and report it.
    pub(crate) fn step(&mut self, text: impl Into<String>) {
        let text = text.into();
        (self.sink)(&ResizeEvent::Step(text.clone()));
        self.operations.push(text);
    }

    /// Report an event that is always reported.
    pub(crate) fn notice(&mut self, event: ResizeEvent) {
        (self.sink)(&event);
    }

    /// Whether detail events reach the caller. Lets a caller skip building
    /// one that would be dropped.
    pub(crate) fn wants_detail(&self) -> bool {
        self.verbose
    }

    /// Report a detail event, if the caller asked for detail.
    pub(crate) fn detail(&mut self, event: ResizeEvent) {
        if self.verbose {
            (self.sink)(&event);
        }
    }

    pub(crate) fn into_operations(self) -> Vec<String> {
        self.operations
    }
}

#[cfg(test)]
impl Reporter<'static> {
    /// A reporter that drops everything, for tests.
    pub(crate) fn silent() -> Self {
        Self::new(Box::leak(Box::new(|_: &ResizeEvent| {})), false)
    }
}
