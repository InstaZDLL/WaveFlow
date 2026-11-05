import {
  remoteGetPlayQueue,
  remoteQueueJump,
  type RemotePlayQueue,
} from "./tauri/remoteServer";
import { playerExternalJump, playerGetExternalQueue } from "./tauri/player";
import { podcastGetQueue, podcastQueueJump } from "./tauri/podcast";

/**
 * The queues that live in memory on the backend rather than in the local
 * `queue_item` table: the remote play queue (RFC-005), the files played
 * without importing them, and a channel's episodes. Same shape, so one view renders either;
 * each is read and jumped through its own commands.
 */
export const DETACHED_QUEUES = {
  remote: { read: remoteGetPlayQueue, jump: remoteQueueJump },
  file: { read: playerGetExternalQueue, jump: playerExternalJump },
  podcast: { read: podcastGetQueue, jump: podcastQueueJump },
} satisfies Record<
  string,
  {
    read: () => Promise<RemotePlayQueue | null>;
    jump: (index: number) => Promise<void>;
  }
>;
