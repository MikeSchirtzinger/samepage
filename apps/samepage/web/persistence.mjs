export function beginPersistenceHandshake(state) {
  return Object.freeze({
    generation: state.generation,
    carriedFailure: state.failure !== null,
    carriedPending: state.pending !== null,
  });
}

export function finishPersistenceHandshake(state, start) {
  if (state.generation !== start.generation) return state;
  return Object.freeze({
    ...state,
    failure: start.carriedFailure ? null : state.failure,
    pending: start.carriedPending ? null : state.pending,
  });
}

export function persistenceFailed(state, message) {
  return Object.freeze({
    failure: message,
    pending: null,
    generation: state.generation + 1,
  });
}

export function persistencePending(state, message) {
  return Object.freeze({
    ...state,
    pending: message,
    generation: state.generation + 1,
  });
}

export function persistenceResolved(state) {
  return Object.freeze({
    ...state,
    pending: null,
    generation: state.generation + 1,
  });
}

export function persistencePresentation(state) {
  if (state.failure) return Object.freeze({
    status: "Save failed",
    banner: `Document persistence failed: ${state.failure}`,
  });
  if (state.pending) return Object.freeze({
    status: "Save still pending",
    banner: `Document save is still pending: ${state.pending}`,
  });
  return Object.freeze({status: null, banner: null});
}
