/**
 * A change the user asked for did not reach the library: the heart or the
 * playlist stays as it was, and `WriteFailedToast` says so.
 *
 * The database can refuse a write it waited five seconds for, while a long
 * writer (a scan) holds it. The command then rejects, and every caller
 * already leaves the view as it was, but most of them only log it, so the
 * click looked as if it had done nothing. Announced here, in the wrappers,
 * so every place that likes a track or fills a playlist is covered without
 * each of them knowing.
 */
export const WRITE_FAILED_EVENT = "waveflow:write-failed";

/** What did not land, for the sentence the toast picks. */
export type WriteFailedKind = "like" | "playlist";

export interface WriteFailedDetail {
  kind: WriteFailedKind;
}

/** `call`'s result, or its rejection after announcing it. */
export async function announceWriteFailure<T>(
  kind: WriteFailedKind,
  call: Promise<T>,
): Promise<T> {
  try {
    return await call;
  } catch (err) {
    window.dispatchEvent(
      new CustomEvent<WriteFailedDetail>(WRITE_FAILED_EVENT, {
        detail: { kind },
      }),
    );
    throw err;
  }
}
