name: Follow connections after rearranging a card map
url: http://127.0.0.1:8098/
workflow:
1. Open a populated card map with multiple relationships entering and leaving the same side of one card. Preserve the person's saved card positions.
2. Verify every relationship has a distinct attachment point. Hollow circles identify the source; arrowheads identify the destination.
3. Verify ordinary relationships do not merge into shared segments, pass underneath cards, or acquire tiny bends between nearly aligned cards.
4. Hover each relationship. Verify that route remains prominent, both endpoint cards visibly gain an outline, and the readout names the source and destination.
5. Click a relationship, move the pointer away, and verify the highlight stays. Press Escape and verify it clears.
6. Reach the same relationship with keyboard focus and press Enter. Verify the same highlight and aria-pressed state.
7. Verify short labels remain on one line and sit on their own route. The minimap remains visible and usable.
8. Move a card and verify its connections reattach to the measured boundary. Reload and verify positions remain saved and routes remain deterministic.
9. Verify connection inspection does not write shared state or move the camera. Require host and browser describe equality after synchronization.
10. Include state-machine transitions in a separate scene. Their distinct curve paths must not reserve invisible orthogonal lanes for ordinary relationships.
