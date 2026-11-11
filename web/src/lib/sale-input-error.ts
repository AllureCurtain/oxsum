/** Validation metadata lets the sale UI translate without changing exact parsing. */
export class SaleInputError extends Error {
  readonly key: string;
  readonly values: Record<string, string | number>;
  constructor(key: string, message: string, values: Record<string, string | number> = {}) {
    super(message);
    this.key = key;
    this.values = values;
  }
}
