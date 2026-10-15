import type { JoinedScheduledMeet } from "./scheduled-meet-presence";

import { executeTransaction } from "~/db";
import { enqueueDatabaseWrite } from "~/db/write-queue";

export async function saveJoinedScheduledMeet(
  sessionId: string,
  joined: JoinedScheduledMeet,
): Promise<void> {
  await enqueueDatabaseWrite(`session:${sessionId}`, async () => {
    const [updated] = await executeTransaction([
      {
        sql: `UPDATE sessions SET metadata_json = json_set(
          CASE WHEN json_valid(metadata_json) THEN metadata_json ELSE '{}' END,
          '$.verified_meet', json(?)
        ), updated_at = ? WHERE id = ? AND deleted_at IS NULL`,
        params: [
          JSON.stringify({
            connection_id: joined.connectionId,
            conference_record: joined.conferenceRecord,
          }),
          new Date().toISOString(),
          sessionId,
        ],
      },
    ]);
    if (!updated) throw new Error("Scheduled meeting session no longer exists");
  });
}
