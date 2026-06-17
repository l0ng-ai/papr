export type ImageBytesResponse = ArrayBuffer | Uint8Array | number[];

export function imageBytes(data: ImageBytesResponse): Uint8Array {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  return new Uint8Array(data);
}
