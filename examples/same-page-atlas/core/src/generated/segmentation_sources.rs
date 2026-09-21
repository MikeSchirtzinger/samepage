// Derived from the checked segmentation source. Run dev/atlas-segmentation-contract generate.
pub const SEGMENTATION_CONTRACT_SHA256: &str =
    "10163d7f54fc9824ddb4bd79325adcfdf74b3f4f47ed9967b8beedf948be12ea";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentSourceContract {
    pub id: &'static str,
    pub target_ref: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub width: u32,
    pub height: u32,
}

pub const DEFAULT_SEGMENT_SOURCE_ID: &str = "mechanical-owl-wing-flap-v1";
pub const SEGMENT_SOURCE_URL: &str = "/assets/visual-instinct-infographic.png";
pub const SEGMENT_SOURCE_SHA256: &str =
    "21e3e650e1dfea74d09d5fca62b1da25431db3b4dac1bb3757f72583e04d917a";
pub const SEGMENT_SOURCE_WIDTH: u32 = 1536;
pub const SEGMENT_SOURCE_HEIGHT: u32 = 1024;

pub const SEGMENT_SOURCES: &[SegmentSourceContract] = &[
    SegmentSourceContract {
        id: "mechanical-owl-wing-flap-v1",
        target_ref: "mechanical-owl",
        url: "/assets/visual-instinct-infographic.png",
        sha256: "21e3e650e1dfea74d09d5fca62b1da25431db3b4dac1bb3757f72583e04d917a",
        width: 1536,
        height: 1024,
    },
    SegmentSourceContract {
        id: "evidence-loop-stages-v1",
        target_ref: "evidence-loop",
        url: "/assets/evidence-loop-infographic.png",
        sha256: "c202a6867f401f943266da0587fdf3ab6925ebc81891edfe862ab6729ee19442",
        width: 1536,
        height: 1024,
    },
    SegmentSourceContract {
        id: "epsilon-hypersphere-lesson-v1",
        target_ref: "epsilon-hypersphere",
        url: "/assets/epsilon-hypersphere-infographic.png",
        sha256: "4c11387459c3d89bc0df5b17d564641cd6a7af94583b8ed2e02fa06d8c0bcde4",
        width: 1536,
        height: 1024,
    },
    SegmentSourceContract {
        id: "epsilon-hypersphere-lesson-v2",
        target_ref: "epsilon-hypersphere-worked-example",
        url: "/assets/epsilon-hypersphere-infographic-v2.png",
        sha256: "df945fefbeef9c05140f9454607e789aadd4fff0f4c74c610ee64292c75ee8f8",
        width: 1536,
        height: 1024,
    },
];

pub fn segment_source_by_id(id: &str) -> Option<&'static SegmentSourceContract> {
    SEGMENT_SOURCES.iter().find(|source| source.id == id)
}

pub fn segment_source_by_identity(
    sha256: &str,
    width: u32,
    height: u32,
) -> Option<&'static SegmentSourceContract> {
    SEGMENT_SOURCES
        .iter()
        .find(|source| source.sha256 == sha256 && source.width == width && source.height == height)
}
