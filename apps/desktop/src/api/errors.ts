/** JSON-RPC style error (see docs/API.md "Errors"). */
export class ApiError extends Error {
  constructor(
    public code: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export const ErrorCodes = {
  notFound: -32004,
  authRequired: -32001,
  unsupportedPlatform: -32005,
} as const;

export function errorMessage(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}
