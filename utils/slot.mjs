import { rpcSubscriptions } from "./rpc.mjs";

const MAX_SLOT_FETCH_ATTEMPTS = process.env.MAX_SLOT_FETCH_ATTEMPTS || 100;

let slotNotifications;
async function* slots() {
  slotNotifications ||= await rpcSubscriptions
    .slotsUpdatesNotifications()
    .subscribe({ abortSignal: AbortSignal.any([]) });
  let attempts = 0;
  for await (const notification of slotNotifications) {
    if (notification.type === "firstShredReceived") {
      attempts = 0;
      yield notification.slot;
      continue;
    }
    if (++attempts >= MAX_SLOT_FETCH_ATTEMPTS) {
      console.log(
        `ERROR: Max attempts for fetching slot type "firstShredReceived" reached, exiting`
      );
      process.exit(0);
    }
  }
}

export async function getNextSlot() {
  const { value: nextSlot } = await slots().next()
  return nextSlot;
}