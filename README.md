# lorai

Projekt protokołu wymiany ocen i zatwierdzania decyzji przez zamkniętą grupę niezależnych węzłów, przeznaczony do pracy na ograniczonym, zawodnym kanale radiowym LoRa.

Status: **projektowanie; brak implementacji protokołu**. Uzgodnienia, granice gwarancji oraz proponowany zakres pierwszego laboratorium opisuje [specyfikacja](docs/superpowers/specs/2026-09-18-lorai-design.md).

## Kontynuowanie pracy w Claude Code

Zacznij od [handoffu](docs/HANDOFF.md) i instrukcji w `CLAUDE.md` / `AGENTS.md`.
[Roadmapa](docs/IMPLEMENTATION-ROADMAP.md) dzieli projekt na etapy z bramkami przeglądu.
[Plan M1](docs/superpowers/plans/2026-09-18-m1-quorum-policy.md) zawiera konkretne pliki, API, kod i testy pierwszego etapu. Jest planem do wykonania, nie raportem z wykonanych testów.

## Granice projektu

- `lorai`: protokół, kontrakt wiadomości, kryptografia, ważone quorum, trwały stan, kolejki i przekazywanie, abstrakcje transportu/zegara oraz symulacja.
- [`morsik-lora`](https://github.com/kamilrybacki/morsik-lora): integracja `lorai` z kontraktami i lokalnym stosem Morsika.
- `morsik-analysis`: pozyskiwanie i przetwarzanie danych oraz cała inferencja modeli.
- `morsik-dashboard`: prezentacja danych i wyników, bez roli koordynatora floty.

`lorai` nie wymaga centralnego serwera podczas działania. Wspólny proces symulatora odtwarza środowisko radiowe, nie podejmuje decyzji za węzły.

Kolejność prac: pełna symulacja → dwa lekkie modele przez integrację → węzły fizyczne. Nie jest to certyfikowany system bezpieczeństwa żeglugi ani dowód prawdziwości ocen modeli.
