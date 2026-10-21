import {
  remoteGetPlayQueue,
  remoteQueueJump,
  type RemotePlayQueue,
} from "./tauri/remoteServer";
import { playerExternalJump, playerGetExternalQueue } from "./tauri/player";

/**
 * The two queues that live in memory on the backend rather than in the
 * local `queue_item` table: the remote play queue (RFC-005), and the files
 * played without importing them. Same shape, so one view renders either;
 * each is read and jumped through its own commands.
 */
export const DETACHED_QUEUES = {
  remote: { read: remoteGetPlayQueue, jump: remoteQueueJump },
  file: { read: playerGetExternalQueue, jump: playerExternalJump },
} satisfies Record<
  string,
  {
    read: () => Promise<RemotePlayQueue | null>;
    jump: (index: number) => Promise<void>;
  }
>;
