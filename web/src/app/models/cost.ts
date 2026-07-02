/** A live cost estimate for a token count (`GET /api/cost`). `usd` is null on a
 *  Claude plan, where a dollar figure would mislead — `subscription_note`
 *  explains why. Mirrors the serve route's `cost_body`. */

export interface CostQuery {
  model: string;
  input: number;
  output: number;
}

export interface CostEstimate {
  usd: number | null;
  subscription_note: string | null;
}
