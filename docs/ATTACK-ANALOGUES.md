# Attacks on consensus systems, and which of them reach this one

The question that prompted this: *what is our 51 % attack, and how did other
systems handle theirs?*

The short answer is that the most-studied attack on open blockchains has no
analogue here, for a reason worth understanding rather than celebrating; that
the attack which *does* reach us is a different one entirely; and that it is
already the highest-rated row in `THREAT-MODEL.md`.

**Status.** Sections 1 to 4 are sourced, with URLs. Section 5 is arithmetic on
this protocol, pinned by tests in `tests/policy.rs`. Section 6 names what this
document does **not** cover, because the research that would have covered it
was cut short; do not mistake its absence for a finding.

---

## 1. First, this is not a blockchain, and that settles most of the list

No chain, so no fork choice and no reorganisation. No transferable asset, so
no double spend. No mining or staking lottery, so no rentable attack capacity.
Membership is a closed list signed by the fleet's administrator, so no Sybil.
The verdict is positive-only: a quorum can say *enough of us endorsed this*
and can never say *this is false*.

| Blockchain attack | Why it does not reach here |
|---|---|
| Double spend | No asset, no balance, no chain of custody |
| Reorg, fork choice | No chain. A case has stages, not competing histories |
| Rented hashpower | Nothing to rent. Capacity is a signed key you do not have |
| Sybil | Fixed membership. The problem **moves** to whoever signs the list |
| Nothing-at-stake, stake grinding | No proposal lottery, no stake |
| Long-range history rewrite | No history to rewrite. Nearest analogue is journal rollback, F18 |
| Maximal extractable value | No ordered, value-bearing transactions |

Naming that honestly matters, because the reader who arrives asking about 51 %
is reasoning from a cost model that does not apply, and the one that does
apply is less comfortable.

---

## 2. Why a blockchain's majority attack is cheap, in the words of the people
   who measured it

This is the part worth transplanting, because it explains what open consensus
buys with all that expenditure, and what it cannot buy.

**The cost of Nakamoto trust is a flow, not a stock.** Budish's central
result, in the published version:

> Nakamoto Trust : p ≥ V_attack
>
> Said differently, in Schelling (1956) the cost of attack is a stock (the net
> present value of the relationship) whereas in Nakamoto (2008) the cost of
> attack is a flow (the recurring costs of honest trust support for a short
> period of time).

— *Trust at Scale: The Economic Limits of Cryptocurrencies and Blockchains*,
Quarterly Journal of Economics 140(1), Feb 2025.
<https://socialsciences.uchicago.edu/sites/default/files/2024-09/Economic%20Limits%20Crypto%20Blockchains%20-%20QJE%20Sept%202024.pdf>

**And the flow can be nearly free.** Theorem 2, same paper:

> If the attacker does not face any cost frictions relative to the costs of
> honest participants, the attack concludes without any difficulty adjustment,
> and the attack does not cause the value of the cryptocurrency to fall, then
> the net cost of attack is zero. [...] In effect, **permissionless consensus
> treats the attacker 'as if' they are an honest participant, because the
> majority determines the truth.**

That last sentence is the whole thing. In an open system the attacker buys
their way into the set that decides, and the protocol cannot tell the
difference, because being in that set *is* the qualification.

**The security bill scales linearly with the prize.** Theorem 1 gives
`p_block > V_attack / (A*·t(A*))`, and the calibration is brutal: the base
case needs a per-block cost of **5 % of the value secured**, which is
**262,801 % per year**. Securing against a $1 billion attack costs $2.63
trillion a year; against $40 billion, more than 2023 world GDP.

> if attacking the system grows 1000 times more attractive, then the cost of
> securing the system must grow 1000 times as well [...] In contrast, in many
> other contexts investments in computer security yield convex returns [...]
> analogously to how a lock on a door increases the security of a house by
> more than the cost of the lock.

**Bribery beats renting, and the bribe is self-financing.** Bonneau's taxonomy
is a two-by-two — rent, build, bribe, buy out — across *new versus existing
capacity* and *temporary versus permanent control*
(<https://fc18.ifca.ai/bitcoin/papers/bitcoin18-final17.pdf>). Bribery wins
because you only need to move half the existing capacity rather than duplicate
all of it: about **$125,000/hr for Ethereum or $250,000/hr for Bitcoin** at
2017 prices, against roughly **$1 million/hr** to rent equivalent GPU capacity
and about **$1.5 billion** to build it.

Worse, in the 2016 paper the bribe need not be paid at all if the attack
fails:

> the attacker only pays if the attack succeeds. Thus, this method inherently
> transfers risk from the attacker to the miners accepting bribes.

— <https://jbonneau.com/doc/B16a-BITCOIN-why_buy_when_you_can_rent.pdf>

**Measured, not just modelled.** Lovejoy monitored 23 coins for up to ten
months and found the theory holds in practice:

> the 51 % attacks we detected were likely break-even or profitable even
> without considering the double-spent transactions they included [...]
> there is no transaction value that is safe from an incentive-compatible
> double-spend attack.

— <https://www.dci.mit.edu/s/LovejoyJamesP-meng-eecs-2020-1.pdf>

For Bitcoin Gold in January 2020 the measured rental cost of a fourteen-block
reorg was **about 0.2 BTC, roughly $1,700**, against roughly the same value
recovered in block rewards — so break-even before the double spend, and the
reorg was deep enough to clear the exchange's twelve-confirmation escrow.
<https://gist.github.com/metalicjames/71321570a105940529e709651d0a9765>

Realised attacks, from the QJE appendix: Ethereum Classic suffered a
**7,000-block** reorganisation in August 2020, about a full day of chain;
Bitcoin Gold lost **$18.6 million** in May 2018, around 74 % of the prior
week's average daily transaction volume.

---

## 3. The defences other systems actually deployed, and how they fared

Two patterns are worth taking, and one is worth refusing.

**Checkpointing works and is an admission.** After the November 2018 Bitcoin
Cash split, Bitcoin ABC shipped a rolling checkpoint that refuses reorgs
deeper than **ten blocks**, and no majority attack materialised even though
Bitcoin SV had signalled **76.39 %** of hashpower before the fork. The
signalled majority evaporated on contact with economics: in the first
twenty-four hours ABC lost **$277,875** and SV **$324,904** mining at a loss.
<https://www.bitmex.com/blog/forkmonitor-info-updated-to-estimate-value-of-mining-losses-since-the-bitcoin-cash-split>

A rolling checkpoint is a finality rule imposed outside the consensus that was
supposed to provide it. That is not a criticism; it is the point. This
protocol has the same shape already: a case reaches a verdict and the verdict
does not get revisited, because there is no longer chain to lose to.

**Difficulty and time rules are a rich source of self-inflicted wounds.**
Bitcoin Cash's emergency difficulty adjustment (August to November 2017)
lowered difficulty 20 % whenever six blocks took twelve hours, which miners
promptly farmed: difficulty fell to **7 % of Bitcoin's**, blocks alternated
between one a minute and none for twelve hours, and the chain built a
**5,000-block lead** over Bitcoin — coins issued years early.
<https://www.bitmex.com/blog/bitcoins-block-timestamp-protection-rules>

Verge is sharper still, and is the incident closest to something we could have
built. The attacker set block timestamps roughly **an hour in the past** so
the retarget believed the algorithm had been idle, driving its difficulty to
the floor, and then mined **about one block per second**. Around **20 million
XVG** in April 2018 and **35 million** in May, the second time by alternating
two algorithms to defeat the first patch.
<https://bitcointalk.org/index.php?topic=3256693.0>

The lesson transfers directly: *a protocol that lets a participant's claimed
clock change what the protocol does has handed that participant a lever*. This
protocol anchors slots on the trigger frame rather than on wall-clock time
(D6), and bounds skew explicitly, precisely so a wrong or lying clock cannot
move a member's slot. That was decided for scheduling reasons; the Verge
incident is the security argument for it.

For completeness: Bitcoin's own time-warp bug has existed since 2009, would
let a majority miner drain the remaining subsidy in **roughly 38 to 40 days**,
and **has never been executed on mainnet** — the setup is public and takes a
month, so it has never been urgent. <https://bip54.org/>

**Censorship is cheap, is not exclusion, and costs the censor.** The largest
real case is Ethereum's OFAC-filtering relays. Measured properly, sanctioned
transactions were **delayed by a median of 11.43 seconds, about one block**,
not excluded. <https://ethresear.ch/t/estimating-inclusion-delays-for-censored-transactions/15115>

And a censoring block earns less: Marathon's one compliant Bitcoin block
carried **178 transactions and earned $2,903**, against **1,180 and 1,096**
transactions and **$17,478 and $17,528** in the two adjacent blocks. Marathon
abandoned the policy **twenty-six days** after the first block.
<https://www.theblock.co/post/104263/an-ofac-compliant-bitcoin-miner-revives-debate-about-transaction-censorship>

The theoretical bound is the one that matters for us: above **50 % of
validators censoring, a proof-of-stake chain cannot achieve censorship
resilience**. <https://ar5iv.labs.arxiv.org/html/2305.18545>

---

## 4. One case where the majority was used on purpose, and returned the money

On 15 May 2019 two Bitcoin Cash pools reorganised the chain two blocks deep to
undo an opportunistic sweep of coins made spendable by that day's hard fork.
Roughly **3,655 BCH, about $1.39 million**, went back to the intended
recipients. BitMEX would not call it coordinated on the record; a
Chinese-language account of it is unambiguous that hashpower was moved from
Bitcoin for the purpose.
<https://www.coinbase.com/blog/a-deep-dive-into-the-recent-bch-hard-fork-incident>

Worth recording because it is the clearest demonstration that in an open
system the majority simply *is* the rule, for good ends as readily as bad.

---

## 5. So what is our 51 % attack

Approval here is the **conjunction** of two thresholds. Count:
`midpoint(n, floor(0.4·n)) + 1`. Competence: `3·support > 2·total`. Both, or
nothing.

| Fleet | To force a verdict | Share | Silent members that block one |
|---|---|---|---|
| 3 | 3 | 100 % | 1 |
| 5 | 4 | 80 % | 2 |
| 6 | 5 | 83 % | 2 |
| 10 | 8 | 80 % | 3 |
| 17 | 12 | 70 % | 6 |
| 64 | 45 | 70 % | 20 |

So the analogue of a 51 % attack is a **70 %-to-unanimous attack on safety**,
and a **one-member-to-30 % attack on liveness**. Seventy per cent is a floor
reached only in larger fleets; a first deployment of three needs unanimity, so
one silent member blocks everything. The liveness margin is worst exactly
where a deployment starts. Both figures are pinned in `tests/policy.rs`, which
corrected an earlier draft of this paragraph that claimed "70 to 80 %".

**Weighting cannot lower the cost of capture here, and that is the structural
difference from a stake-weighted chain.** Because approval is an AND, the
competence threshold can only add a requirement on top of the headcount. Under
the 3:1 ratio cap the competence threshold is reachable by **at least as few**
members as the count threshold needs — checked exhaustively for every fleet
from 3 to 64 — so it never lets an attacker succeed with a smaller group. In a
stake-weighted chain, weight **replaces** headcount, so concentration lowers
the number of entities an attacker must be.

**Now apply Bonneau's two-by-two, which is the useful part:**

| | Reaches us? |
|---|---|
| **Rent** | No. There is no capacity market. Membership is a key you hold or do not |
| **Build** | No. You cannot manufacture a seat |
| **Bribe** | **Yes**, and it is the real quorum attack: suborn 70 % of vessel operators |
| **Buy out** | **Yes, and it is far cheaper** — see below |

Bribery here is materially worse for the attacker than Bonneau's version, and
the reason is structural: there are no block rewards, so there is no in-band
channel through which a bribe can be offered, and nothing that makes the bribe
*self-financing*. Bonneau's attacker pays only if the attack succeeds; ours
must pay operators out of band, in advance, with no protocol mechanism to
escrow the payment against success. Budish's zero-net-cost theorem simply does
not apply, because the protocol does not pay participants and therefore cannot
reimburse an attacker for pretending to be one.

**But the cheapest attack is not against the quorum at all.** It is the
administrator's key. Whoever holds it writes the manifest, and the manifest
decides who the members are, what each is worth, and which epoch is current.
That is one key, entered by hand onto each vessel (D26), verified against a
file that sits on the same filesystem as the manifest it checks. No amount of
quorum arithmetic touches it. It is `THREAT-MODEL.md` **F21**, it is rated
High, and this research does not lower it — it raises its relative importance,
because it is now clearly the cheapest way in.

That is the honest summary. **A permissionless chain has no single key whose
holder can redefine the membership; we do.** What we get in exchange is that
nobody can buy their way into the set that decides, and the security bill does
not scale with the value of what is being decided.

---

## 6. What this document does not cover

The research that would have filled these was cut off by a provider quota
limit part-way through. Their absence is a gap, not a finding:

- **Classical BFT thresholds** — the `n ≥ 3f+1` bound, what breaks first when
  `f` is exceeded (safety versus liveness), and the precise statement of what
  an adversary holding exactly `f`, `f+1` and `2f+1` can do. This protocol's
  40 % fault budget is stricter than the classical third; that choice deserves
  a sourced justification and does not have one here.
- **Equivocation handling in production systems** — Tendermint evidence
  gossip and slashing, Ethereum's surround-vote conditions and correlation
  penalty, and the accountable-safety literature. Directly relevant: this
  protocol records equivocation evidence and acts on none of it (F5), and the
  question of what a fleet with no stake to slash can actually do about a
  member caught equivocating is open.
- **Eclipse and partition attacks on small validator sets**, and whether the
  wireless and ad-hoc radio literature has anything to say about a consensus
  protocol whose entire network is one shared broadcast medium with no
  routing. This is the least-covered and most relevant gap.
- **Rollback of durable state in permissioned systems** — whether any
  production system defines a rule for a node restored from backup. F18 says
  no persisted state can detect its own restoration; it would be worth knowing
  whether anybody else has done better.
