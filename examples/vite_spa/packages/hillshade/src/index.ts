import wasmUrl from './generated/shade.wasm?url';

// The exports of `shade`, the Rust binary of this package, built for the browser.
interface Shade {
  memory: WebAssembly.Memory;
  alloc(bytes: number): number;
  dealloc(ptr: number, bytes: number): void;
  shade(heights: number, width: number, height: number, out: number): void;
}

// The URL changes when the module does, and with it this module: the page gets the new one.
const instance = WebAssembly.instantiateStreaming(fetch(wasmUrl), {}).then(
  ({ instance }) => instance.exports as unknown as Shade,
);

/** Shaded relief of a heightmap of `width * height` heights, row by row, as an image. */
export async function hillshade(heights: ArrayLike<number>, width: number, height: number): Promise<ImageData> {
  const wasm = await instance;
  const cells = width * height;
  const input = wasm.alloc(cells * 4);
  const output = wasm.alloc(cells * 4);
  try {
    // In thousands of metres: the crate's scale.
    new Float32Array(wasm.memory.buffer, input, cells).set(Array.from(heights, (h) => h / 1000));
    wasm.shade(input, width, height, output);
    return new ImageData(new Uint8ClampedArray(wasm.memory.buffer.slice(output, output + cells * 4)), width, height);
  } finally {
    wasm.dealloc(input, cells * 4);
    wasm.dealloc(output, cells * 4);
  }
}
