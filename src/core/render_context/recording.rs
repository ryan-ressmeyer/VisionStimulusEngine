use super::*;

impl<'a> RenderContext<'a> {
    /// Record per-frame experimental data merged with the most recent flip's timing.
    ///
    /// In synchronous `run()`, call after `flip()`. The receipt contains the best timestamp VSE
    /// obtained after processing completion and presentation feedback.
    ///
    /// In `run_buffered()`, call inside the confirmation callback. Inspect the delivered
    /// `FlipInfo::timing_source`; a retired buffered submission need not have scanout feedback.
    ///
    /// The data struct must implement `serde::Serialize`. Multiple calls per frame
    /// are allowed — each produces one row keyed to the same `frame_number`.
    ///
    /// # Errors
    ///
    /// - [`VSEError::NoSession`] if no session was attached to the builder.
    /// - [`VSEError::NoFlipPending`] if called before `flip()` in synchronous mode.
    /// - [`VSEError::NoConfirmedFlip`] if called from a buffered render callback.
    pub fn record_frame<F: serde::Serialize>(&mut self, data: F) -> Result<(), VSEError> {
        // Buffered mode: use the confirmed flip set by run_buffered() before this callback.
        // A headless session has no present target and is never buffered.
        let in_buffered_mode = self
            .state
            .target
            .present()
            .is_some_and(|p| p.in_buffered_mode);
        if in_buffered_mode {
            let flip = self
                .state
                .target
                .present_expect()
                .buffered_confirmed_flip
                .clone()
                .ok_or(VSEError::NoConfirmedFlip)?;

            let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;

            let payload =
                serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;

            let frame_number = flip.frame_number;
            recording
                .session
                .send_frame(FrameMessage {
                    flip,
                    payload: Some(payload),
                    schema_name: std::any::type_name::<F>(),
                })
                .map_err(|e| VSEError::DataRecording(e.to_string()))?;
            recording.last_claimed_frame = Some(frame_number);
            return Ok(());
        }

        // Synchronous mode: use pending_flip from the most recent flip().
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let flip = recording
            .pending_flip
            .clone()
            .ok_or(VSEError::NoFlipPending)?;

        recording.last_claimed_frame = Some(flip.frame_number);

        let payload =
            serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;

        recording
            .session
            .send_frame(FrameMessage {
                flip,
                payload: Some(payload),
                schema_name: std::any::type_name::<F>(),
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;

        Ok(())
    }

    /// Record a typed annotation at the current timestamp.
    ///
    /// `stream` is the table/group name in the output file (e.g. `"trial"`,
    /// `"subject_info"`, `"calibration"`). Any `serde::Serialize` type is accepted.
    pub fn record_annotation<A: serde::Serialize>(
        &mut self,
        stream: &str,
        data: A,
    ) -> Result<(), VSEError> {
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let timestamp = self.state.clock.now();
        let payload =
            serde_json::to_vec(&data).map_err(|e| VSEError::DataRecording(e.to_string()))?;
        recording
            .session
            .send_annotation(crate::data::messages::AnnotationMessage {
                stream: stream.to_string(),
                timestamp,
                payload,
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;
        Ok(())
    }

    /// Record a raw key-value event at the current timestamp.
    ///
    /// Use for unstructured or one-off data. For structured, repeated data
    /// prefer [`Self::record_frame`] or [`Self::record_annotation`].
    pub fn record_event(&mut self, name: &str, value: &str) -> Result<(), VSEError> {
        let recording = self.state.recording.as_mut().ok_or(VSEError::NoSession)?;
        let timestamp = self.state.clock.now();
        recording
            .session
            .send_event(crate::data::messages::EventMessage {
                name: name.to_string(),
                timestamp,
                value: value.to_string(),
            })
            .map_err(|e| VSEError::DataRecording(e.to_string()))?;
        Ok(())
    }
}
