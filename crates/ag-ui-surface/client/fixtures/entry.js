export function activate({ mount, wasm }) { mount.textContent = `fixture ${wasm.abi()}`; return wasm; }
