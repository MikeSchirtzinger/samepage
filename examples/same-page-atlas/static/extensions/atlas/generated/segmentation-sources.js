// Derived from the checked segmentation source. Run dev/atlas-segmentation-contract generate.
export const SEGMENTATION_CONTRACT_SHA256 = "10163d7f54fc9824ddb4bd79325adcfdf74b3f4f47ed9967b8beedf948be12ea";
export const DEFAULT_SEGMENT_SOURCE_ID = "mechanical-owl-wing-flap-v1";

function deepFreeze(value) {
  if (!value || typeof value !== "object" || Object.isFrozen(value)) return value;
  for (const child of Object.values(value)) deepFreeze(child);
  return Object.freeze(value);
}

export const SEGMENT_SOURCES = deepFreeze({
  "mechanical-owl-wing-flap-v1": {
    "id": "mechanical-owl-wing-flap-v1",
    "targetRef": "mechanical-owl",
    "url": "/assets/visual-instinct-infographic.png",
    "sha256": "21e3e650e1dfea74d09d5fca62b1da25431db3b4dac1bb3757f72583e04d917a",
    "size": [
      1536,
      1024
    ]
  },
  "evidence-loop-stages-v1": {
    "id": "evidence-loop-stages-v1",
    "targetRef": "evidence-loop",
    "url": "/assets/evidence-loop-infographic.png",
    "sha256": "c202a6867f401f943266da0587fdf3ab6925ebc81891edfe862ab6729ee19442",
    "size": [
      1536,
      1024
    ]
  },
  "epsilon-hypersphere-lesson-v1": {
    "id": "epsilon-hypersphere-lesson-v1",
    "targetRef": "epsilon-hypersphere",
    "url": "/assets/epsilon-hypersphere-infographic.png",
    "sha256": "4c11387459c3d89bc0df5b17d564641cd6a7af94583b8ed2e02fa06d8c0bcde4",
    "size": [
      1536,
      1024
    ]
  },
  "epsilon-hypersphere-lesson-v2": {
    "id": "epsilon-hypersphere-lesson-v2",
    "targetRef": "epsilon-hypersphere-worked-example",
    "url": "/assets/epsilon-hypersphere-infographic-v2.png",
    "sha256": "df945fefbeef9c05140f9454607e789aadd4fff0f4c74c610ee64292c75ee8f8",
    "size": [
      1536,
      1024
    ]
  }
});

export function segmentSource(id) {
  const source = SEGMENT_SOURCES[id];
  if (!source) throw new Error(`Unknown generated Atlas segmentation source ${id ?? "<missing>"}.`);
  return source;
}

export function segmentSourceByIdentity(sha256, size) {
  const source = Object.values(SEGMENT_SOURCES).find((candidate) =>
    candidate.sha256 === sha256
      && Array.isArray(size)
      && size.length === 2
      && candidate.size[0] === size[0]
      && candidate.size[1] === size[1]);
  if (!source) throw new Error("The segment source is not in the generated Atlas catalog.");
  return source;
}
