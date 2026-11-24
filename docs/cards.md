# Cards on file

Many vendors keep one of the company's cards and charge it themselves: a
hauler bills each job to the card it holds. Payvy never moves money, and here
it doesn't instruct the bank either. The vendor pulls the money. Payvy's part
is to know which card each vendor keeps, read that card's charges from the
bank, say which bill each charge paid when the answer is plain, and show a
person the rest. Commands are `bank.cards`, `vendor.link_card`,
`vendor.unlink_card`, `card.sync`, `card.charges`, `card.match_charge` and
`card.dismiss_charge` (`crates/payvy-server/src/commands/cards.rs`); the
matching rule is `payvy_domain::cards::plain_matches`.

Mercury is the first bank. Its API lists the company's cards and reports each
card charge with the card's id ([cards](https://docs.mercury.com/reference/listcards),
[transactions](https://docs.mercury.com/reference/listtransactions), checked
2026-11-24).

## Setup

1. The connected Mercury token must be allowed to **read cards** (and, as
   always, transactions). Mercury can't add a permission to an existing
   token: make a new one with it and reconnect. Without it, `bank.cards`
   says so.
2. On the vendor's page, **Card on file → Link a card…**: choose the card the
   vendor keeps and, optionally, the day its charges count from (default:
   30 days back; earlier charges are history Payvy leaves alone). Only a
   person links, changes or removes a card (`vendor.link_card`,
   `vendor.unlink_card`), and each change is an event naming the card it
   replaced.

Payvy keeps Mercury's card id and the last four digits, never a card number;
Mercury's API doesn't return one. A card is on file with one vendor at a
time, and a vendor has one card: a card shared by several vendors can't say
which vendor a charge paid. A card linked under a bank connection that has
since been replaced doesn't count.

## What changes for the vendor's bills

- **Release sends nothing.** Releasing a bill (or reaching an automatic pay
  step) clears it for the vendor to charge. The preview says
  "••4242 … charges the card on file". This holds even if the vendor also has
  an accepted payee: with a card on file, a transfer would pay twice. Remove
  the card to pay the payee again.
- **The charge marks it paid.** When a charge on the card pays a bill and has
  posted, and the bill is released (or at an automatic pay step), the bill is
  marked paid by the agent `bank`: method `card`, the bank's transaction id as
  the reference, the posting date as the paid date. The ledger follows as for
  any payment made outside Payvy.
- **Charged before release.** Vendors often charge first. The charge is
  paired with the bill and shown as charged before release; the bill is
  marked paid the moment a person releases it. The approval workflow still
  runs — but the money already left, so this is where a person looks.
- **Reversed.** A charge that is later cancelled or reversed at the bank
  reopens the bill it paid, waiting for a person to release it, like a
  returned ACH.

## When Payvy pairs a charge with a bill

Only when the answer is plain:

- the charge has **posted** (pending charges are shown, not matched),
- it is a charge, not a refund,
- exactly one open bill of that vendor, not already paid by a charge, has
  **the same amount to the cent** and an invoice date from 14 days after the
  charge to 60 days before it, and
- no other unmatched charge of that vendor fits that bill.

Two $750 bills, two $750 charges, or a charge a cent off are left for a
person: `card.match_charge` (same vendor, same amount; one charge pays one
whole bill) or `card.dismiss_charge` with a reason (a fee, a refunded
mistake). Never widen the rule to the nearest date or a partial amount.
`card.charges { "open": true }` lists what needs a person.

## When it runs

The payment worker runs `card.sync` every five minutes, as the agent `bank`,
for companies with a card on file. Anyone may run it to check now. Each run
asks Mercury for the cards' transactions since the earliest "charges from"
day, records new and changed charges (`card.charge_pending`,
`card.charge_posted`, `card.charge_reversed`), pairs the plain ones
(`card.charge_matched`, rule `amount`) and marks cleared bills paid
(`bill.paid`, `via: card`). A charge is recorded once per bank transaction,
so running it again changes nothing.

## Not yet

- Issuing a new card from Payvy (a vendor-locked card, or one per purchase
  order capped at its amount). Mercury's API can create cards; that is a
  confirmed, person-only command still to build.
- Cards at Ramp, Brex or Rho.
- Mercury webhooks nudging `card.sync` (today it polls).
