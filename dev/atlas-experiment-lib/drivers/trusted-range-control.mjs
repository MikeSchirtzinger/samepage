export async function run(api, input) {
  await api.screenshot("00-loaded.png");
  const observation = await api.trustedRange(input.selector, input.fraction, input.steps);
  const numericValue = Number(observation.value);
  if (!Number.isFinite(numericValue) || numericValue < input.minimum_value) {
    throw new Error(`trusted range value ${observation.value} did not reach ${input.minimum_value}`);
  }
  if (!observation.events.some((event) => event.type === "input" && event.isTrusted === true)) {
    throw new Error("trusted input event was not recorded");
  }
  await api.screenshot("01-after-trusted-drag.png");
  return {
    status: "instrument-positive-control",
    claim_boundary: "This proves the direct CDP trusted-input instrument only. It is not Atlas proof.",
    observation,
  };
}
