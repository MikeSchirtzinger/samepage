function assert(condition, message) {
  if (!condition) throw new Error(message);
}

export async function run(api) {
  await api.setViewport(1440, 900);
  await api.waitFor(`(() => window.atlas?.schema?.ready === true
    && document.querySelector("#sync-state")?.textContent === "replica live"
    && Boolean(window.atlas?.explanation?.snapshot?.()))()`, {
    timeoutMs: 30_000,
    retryOnError: true,
  });
  // A viewport change legitimately schedules a learner-mode refit. Let that
  // layout-owned camera change finish before establishing the motion
  // baseline, so a replay audit cannot confuse responsive fitting with an
  // animation moving the camera.
  await api.sleep(500);
  const motion = await api.evaluate(`(() => {
    const owner = window.atlas.segment.snapshot().find((shape) => shape.segment_motion);
    if (!owner) return null;
    const program = owner.segment_motion;
    return {
      ownerId: owner.id,
      label: program.label,
      tracks: program.tracks.map((track) => ({
        label: track.label,
        targetIds: track.target_ids,
        durationMs: track.duration_ms,
        delayMs: track.delay_ms || 0,
        loop: track.loop,
      })),
    };
  })()`);
  assert(motion?.tracks?.length > 0, "the audit page has no stored motion program");
  const before = await api.evaluate(`(() => ({
    camera: { ...window.atlas.camera },
    running: [...document.querySelectorAll(".segment-object")]
      .flatMap((element) => element.closest("g.shape")?.getAnimations() || [])
      .filter((animation) => animation.id?.startsWith("atlas-motion:") && animation.playState === "running").length,
    activeBodies: document.querySelectorAll('.segment-object[data-motion-active="true"]').length,
  }))()`);
  assert(before.running === 0 && before.activeBodies === 0,
    "stored motion autoplayed before an explicit lesson action");
  await api.screenshot("01-motion-idle.png");
  const replayed = await api.evaluate(`window.atlas.segment.replay(${JSON.stringify(motion.ownerId)})`);
  assert(replayed === motion.tracks.length,
    `explicit replay started ${replayed} tracks instead of ${motion.tracks.length}`);
  const playing = await api.waitFor(`(() => {
    const bodies = [...document.querySelectorAll('.segment-object[data-motion-active="true"]')];
    const samples = bodies.map((body) => ({
      id: body.dataset.segmentId,
      label: body.dataset.motionTrack,
      transform: getComputedStyle(body.closest("g.shape")).transform,
      opacity: getComputedStyle(body).opacity,
    }));
    return samples.length > 0 ? samples : false;
  })()`, { timeoutMs: 2_000, retryOnError: true });
  await api.sleep(450);
  const moved = await api.evaluate(`(() => [...document.querySelectorAll('.segment-object[data-motion-active="true"]')]
    .map((body) => ({
      id: body.dataset.segmentId,
      label: body.dataset.motionTrack,
      transform: getComputedStyle(body.closest("g.shape")).transform,
      opacity: getComputedStyle(body).opacity,
    })))()`);
  assert(moved.some((sample) => sample.transform !== "none"),
    "explicit motion produced no measurable target transform");
  await api.screenshot("02-motion-playing.png");
  const maximumEndMs = Math.max(...motion.tracks.map((track) => track.delayMs + track.durationMs));
  await api.sleep(maximumEndMs + 700);
  const settled = await api.waitFor(`(() => {
    const running = [...document.querySelectorAll(".segment-object")]
      .flatMap((element) => element.closest("g.shape")?.getAnimations() || [])
      .filter((animation) => animation.id?.startsWith("atlas-motion:") && animation.playState === "running").length;
    const activeBodies = document.querySelectorAll('.segment-object[data-motion-active="true"]').length;
    return running === 0 && activeBodies === 0 ? true : false;
  })()`, { timeoutMs: 5_000, retryOnError: true });
  const after = await api.evaluate(`(() => ({
    camera: { ...window.atlas.camera },
    redundantOverlays: [...document.querySelectorAll('.segment-body-canvas[data-redundant-source-overlay="true"]')]
      .map((canvas) => ({ id: canvas.closest(".segment-object")?.dataset.segmentId, opacity: getComputedStyle(canvas).opacity })),
  }))()`);
  assert(after.redundantOverlays.every((entry) => entry.opacity === "0"),
    "an animated segment remained visible after playback settled");
  assert(["x", "y", "scale"].every((key) => Math.abs(after.camera[key] - before.camera[key]) < 0.001),
    "explicit segment motion moved the lesson camera");
  await api.screenshot("03-motion-settled.png");
  return {
    status: "motion-is-explicit-finite-and-returns-to-base-composition",
    motion,
    replayed,
    before,
    playing,
    moved,
    settled,
    after,
  };
}
