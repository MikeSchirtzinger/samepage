name: Compare a proposed relationship with the current map
url: http://127.0.0.1:8098/
workflow:
1. Open a populated card relationship map and read its project facts in the header.
2. Open Compare and create a new proposal. Verify no changes or affected components.
3. Remove a relationship in the proposal. Verify its endpoints and incoming dependency paths are highlighted while the current map stays unchanged.
4. Add a different relationship. Verify green addition and red removal in both the diagrams and table.
5. Name the proposal. Verify the shared confirmation and retained earlier revisions.
6. Select the original revision. Verify it still has no changes.
7. Reload and verify all revisions remain available.
8. Click an affected component. Verify the comparison closes and the component is selected on the shared canvas. Open Review and verify its contents load.
9. Change the current map through an attached agent. Open Compare and verify a stale-baseline warning appears.
10. Repeat the comparison at mobile width and with Paper theme. Verify controls, text and facts remain accessible without horizontal scrolling.
11. Compare the browser architecture projection and describe output with the host endpoints. Require exact equality after synchronization settles.
