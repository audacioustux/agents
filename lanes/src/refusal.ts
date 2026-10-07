// A refused command, in its own module so the modules that throw it need not import the runner.

/** A refused command (exit status `exit`); a retryable refusal keeps a queued lane waiting, any other drops it. */
export class Refusal extends Error {
  constructor(message: string, readonly retryable = false, readonly exit = 1) {
    super(message);
  }
}
