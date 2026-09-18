# lcq — projekt protokołu i pierwszego laboratorium

Data: 2026-09-18.

Status: zapis wymagań uzgodnionych z użytkownikiem, do przeglądu przed planem implementacji. Dokument nie oznacza uruchomienia ani zweryfikowania protokołu. Sekcja „Proponowane doprecyzowania” zawiera rozwiązania techniczne wymagające zatwierdzenia w przeglądzie; pozostałe sekcje odzwierciedlają podjęte decyzje oraz ich konieczne ograniczenia.

## 1. Cel i odpowiedzialności

Zamknięta, z góry znana grupa niezależnych węzłów wymienia krótkie oceny wspólnego zdarzenia przez zawodny, współdzielony kanał radiowy. Po jednej konsultacji może powstać podpisane poparcie spełniające dwa progi: liczby jednostek i ich wag. Nie ma centralnego serwera, wyboru członków przez otwarte discovery, kopania bloków ani uzgadniania globalnej kolejności wszystkich zdarzeń.

| Projekt/moduł | Odpowiedzialność |
|---|---|
| `lcq` | Kontrakt protokołu, tożsamość, kryptografia, etapy oceny, ważone quorum, trwały stan, kolejki, store-and-forward, adaptery transportu, zegar i symulacja |
| `morsik-lora` | Wykorzystanie `lcq`: mapowanie danych Morsika, lokalna integracja HTTP/MQTT, przekazanie konsultacji do analizy, publikacja statusów |
| `morsik-analysis` | Źródła, ekstrakcja, korelacja, deterministyczny scoring, niezależna ocena modelu i jednorazowa ponowna ocena |
| `morsik-dashboard` | Lokalna prezentacja ostrzeżeń, ocen, statusów i łączności; nie jest koordynatorem |

Rdzeń `lcq` nie uruchamia LLM i nie zależy od morskich kategorii ostrzeżeń, HTTP, MQTT ani interfejsu użytkownika. Aplikacja dostarcza ustrukturyzowaną ocenę przez adapter. Gotowe biblioteki realizują kryptografię; nie tworzymy własnych algorytmów kryptograficznych.

W każdej jednostce Morsika jest lokalny Mosquitto. Nie ma wspólnego brokera floty ani mostkowania brokerów między jednostkami. Zdarzenia modułów przechodzą przez MQTT, odczyt stanu przez HTTP. Między jednostkami wolno komunikować się wyłącznie przez adapter radiowy/symulowany `lcq`.

## 2. Etapy projektu

1. Pełna symulacja: programowane oceny, prawdziwe podpisy i szyfrowanie; początkowo 5, następnie 10 i 20 węzłów.
2. Integracja z dwoma bardzo lekkimi modelami działającymi lokalnie. To test kontraktu i konsultacji modeli, nie dowód odporności pięciowęzłowej floty. Polityka dwuwęzłowej próby musi być osobnym manifestem, bez udawania tolerancji dwóch napastników.
3. Fizyczny transport radiowy: pomiary, kalibracja modelu kanału i weryfikacja adaptera.

Implementacja pierwszego etapu nie obejmuje przebudowy istniejącego Morsika, wdrożenia na statkach, gotowej obsługi Meshtastic ani firmware fizycznego radia.

## 3. Zdarzenia, dane i znaczenie wyniku

Pierwszy scenariusz aplikacyjny: przeszkoda/niebezpieczny obszar, np. dryfujący obiekt. Każda jednostka niezależnie dostaje ostrzeżenie z systemu wejściowego, z uzgodnionym identyfikatorem. Może mieć dodatkowo własne obserwacje. Przez radio nie rozsyłamy pełnej treści ostrzeżenia ani promptów.

Każda sprawa jest związana z przestrzenią identyfikatorów, ID zdarzenia, wersją i skrótem wspólnej treści. Skrót dotyczy uzgodnionych danych wejściowych, nie lokalnego podsumowania modelu. Różne treści i wersje nie dzielą głosów.

Opinie: poparcie, zakwestionowanie, brak wystarczających danych. Wyłącznie poparcie może przyczynić się do zatwierdzenia. Nie istnieje flotowe zatwierdzenie „zagrożenia nie ma”. Brak zatwierdzenia nie oznacza bezpieczeństwa.

Dwa poziomy wyniku:

1. Zatwierdzone przez flotę: spełnione oba progi poparcia.
2. Zatwierdzone z poparciem niezależnych obserwacji: dodatkowo minimum dwa niezależne źródła pierwotne wspierające tę samą wersję ostrzeżenia.

Rozróżniamy własne obserwacje i pośrednie poparcie. Liczymy pochodzenie dowodów, nie kopie ani liczbę analizujących modeli. Wymagane są referencje do źródeł, obserwacji i relacji pochodzenia; nieznane pochodzenie nie zwiększa niezależności. Podpis jednostki potwierdza autora deklaracji, nie faktyczną niezależność lub prawdziwość jej danych. W scenariuszu symulator zna prawdziwe pochodzenie i mierzy błędne uznanie niezależności; w produkcji wymaga ona zaufanych metadanych źródłowych.

## 4. Członkostwo, modele i wagi

Manifest misji ustala skład, identyfikatory, klucze publiczne, uprawnienia, dozwolone profile modeli, politykę wag i protokołu. Podpisuje go administrator offline. Discovery nie daje prawa głosu. W pierwszej wersji zmiana członkostwa i odwołanie kluczy odbywają się kontrolowanie poza kanałem radiowym.

Profil modelu zawiera rodzinę/wersję, liczbę parametrów, kwantyzację i odniesienie do konkretnego artefaktu. Ocena wskazuje profil oraz parametry i kwantyzację w zwartym kodowaniu, sprawdzane względem manifestu. Uczestnik nie dostaje większej wagi przez samodzielną deklarację mocniejszego modelu. Podpis nie stanowi atestacji rzeczywiście wykonanego modelu.

Uzgodniona postać wagi surowej:

`r_i = (P_i / 1 miliard)^alpha * q_i`, domyślnie `alpha = 0.5`.

`q_i` pochodzi z jawnej tabeli profili kwantyzacji. To dostrajalna heurystyka, nie miara trafności ani proporcja do liczby bitów. Końcowe dodatnie wagi mają stosunek największej do najmniejszej nie większy niż 3:1. Kilka modeli jednego członka nie tworzy wielu niezależnych głosów. Waga członka jest jednoznacznie ustalona w manifeście, nie zależy od chwilowej dostępności modelu/sąsiadów; niedozwolony fallback nie dziedziczy wagi mocniejszego modelu.

Wagi i progi nie zmieniają się w trakcie misji. Brak łączności nie usuwa członków z mianownika.

## 5. Quorum i granice gwarancji

Niech `N` oznacza liczbę członków, `beta = 0.4` domyślny budżet przejęcia, a `f = floor(beta * N)` maksymalną liczbę nieuczciwych członków.

Minimalna liczba podpisów:

`q_count = floor((N + f) / 2) + 1`.

To dokładna reguła całkowitoliczbowa wynikająca z warunku przecięcia `2*q_count - N > f`. Dla N=5,10,20,100 progi wynoszą odpowiednio 4,8,15,71. Zaokrąglenie budżetu napastników jest jawne; nie zaokrąglamy progów w dół do słabszej gwarancji.

Ten sam zbiór różnych uprawnionych podpisujących musi reprezentować **więcej niż 2/3 całkowitej wagi manifestu**. Przy wagach całkowitoliczbowych warunek ma postać `3 * support_weight > 2 * total_weight`, bez błędów zmiennoprzecinkowych na granicy. Nie sumujemy wagi niepodpisujących członków ani opinii z pierwszego etapu.

Warunek przecięcia jest własnością zbiorów podpisujących, a nie pełnym dowodem protokołu BFT. Wymaga trwałego zakazu sprzecznego podpisania przez uczciwy węzeł i jednoznacznego kontekstu sprawy. W wersji v1 zatwierdzamy tylko poparcie; nie projektujemy konsensusu nad uporządkowanym rejestrem.

Nie gwarantujemy postępu przy 40% milczących napastników, podziale sieci ani jammingu. Nawet jeden milczący węzeł o dużej wadze może zablokować próg wagowy. Przykład wag 3,1,1,1,1: pozostali mają 4/7 < 2/3. Raport pokazuje możliwość blokowania każdej konfiguracji, zamiast deklarować odporność wynikającą wyłącznie z liczby członków.

Bezpieczeństwo kryptograficzne i poprawne liczenie głosów nie zapewniają prawdziwości ostrzeżenia. Uczciwe modele mogą być błędne lub sugerować się tą samą nieprawdziwą informacją.

## 6. Etapy oceny i czas

Jedna wersja ostrzeżenia przechodzi przez niezależną ocenę, zebranie ocen sąsiadów, dokładnie jedną ponowną ocenę i ewentualny wiążący głos popierający. Niezależne oceny, konsultacje i wiążące głosy to różne typy wiadomości. Nie mieszamy ich przy obliczaniu quorum.

Model nie podpisuje. Deterministyczny moduł sprawdza treść kontraktu, uprawnienia, wersję, etap, ważność oraz historię własnych głosów. Ponowna opinia nie odwołuje automatycznie wcześniej wydanego wiążącego głosu. Przy restarcie wznawiamy zapisany etap, zamiast otwierać nową konsultację.

Domyślne terminy względem wspólnego `evaluation_started_at`:

- zebranie ocen do konsultacji: 5 minut;
- oczekiwane zatwierdzenie: 10 minut;
- ważność: z systemu wejściowego, przy jej braku testowo 30 minut;
- ważność zawsze ma pierwszeństwo nad terminami.

Po 10 minutach bez quorum stan pozostaje niepotwierdzony; zbieranie wiążących głosów trwa do wygaśnięcia, bez kolejnej konsultacji. Spóźniona opinia trafia do dziennika, nie zmienia zamkniętego zbioru konsultacji. Wygaśnięcie kończy możliwość aktywnego zatwierdzenia; historyczne dowody pozostają w archiwum. Zatwierdzenie przy różnych podzbiorach głosów nie wymaga identycznych bajtowo pakietów dowodowych, tylko tych samych reguł i odniesienia do sprawy.

Zegary synchronizujemy przed misją; modelujemy ograniczony błąd, dryf i restarty. Przekazanie nie przedłuża podpisanej ważności. Gdy niepewność czasu nie pozwala bezpiecznie stwierdzić ważności, węzeł nie wydaje wiążącego głosu. Nadal pokazuje lokalne ostrzeżenie.

## 7. Komunikaty i transport

Mały, samodzielny pakiet podstawowy zawiera odniesienie do zdarzenia/wersji/treści, etap, werdykt, profil modelu, kontekst nadawcy i pełne zabezpieczenia. Osobne, ograniczone uzupełnienia przenoszą ustrukturyzowane przesłanki: kody powodów, identyfikatory źródeł/obserwacji, czas i jakość danych. Nie wymieniamy swobodnych instrukcji tekstowych dla modeli.

Uzupełnienie jest jednoznacznie związane z konkretną oceną, ma własne uwierzytelnienie i ważność. Podstawowe zatwierdzenie może opierać się na kompletnych głosach podstawowych; brak dowodów nie może podnieść wyniku do poziomu niezależnych obserwacji. Nie ma dowolnego dzielenia nieograniczonych dokumentów na fragmenty.

Odbiorca sam weryfikuje podpisy stanowiące poparcie. Pakiet sąsiada „quorum osiągnięte” jest co najwyżej wskazówką do pozyskania brakujących głosów, nie dowodem. Nie wymuszamy umieszczenia wszystkich podpisów floty w jednej ramce.

Rdzeń ma wymienny adapter transportu. Pierwszy odtwarza kanał LoRa; fizyczny adapter i ewentualny Meshtastic podlegają późniejszej kwalifikacji. Limity payloadu dotyczą całej zakodowanej wiadomości z kryptografią i narzutem konkretnego adaptera. Nie zakładamy stałego czasu transmisji ani zerowego kosztu ramek sterujących.

## 8. Bezpieczeństwo

Każdy członek ma osobny klucz prywatny do podpisów i manifest kluczy publicznych. Flota posiada wspólny sekret szyfrowania dla danej generacji/misji. W pakiecie występuje identyfikator klucza, nigdy sekret. Grupowe szyfrowanie nie zastępuje indywidualnych podpisów.

Pierwsza wersja nie zapewnia forward secrecy: wyciek sekretu grupy może ujawnić wcześniej zapisane transmisje tej generacji. Nie daje natomiast możliwości podrobienia cudzych podpisów bez odpowiedniego klucza prywatnego. Po wykluczeniu członka nowego sekretu nie wolno rozsyłać wyłącznie pod starym wspólnym kluczem.

Autentykacja obejmuje wersję protokołu, manifest/politykę, misję, nadawcę, typ/etap wiadomości, identyfikator i wersję sprawy, treść i ważność. Unikalność nonce oraz ochrona przed replay muszą przetrwać restarty i równoległe wysyłanie. Dopuszczamy opóźnione dostawy poza kolejnością w ograniczonym, ważnym czasowo zakresie; sam maksymalny widziany licznik nie może kasować wszystkich starszych, ale jeszcze nieodebranych pakietów store-and-forward.

Ataki: obcy nadawca, podszycie, replay, modyfikacja, jeden przejęty członek, dwóch współpracujących oraz do `floor(0.4*N)` przy skalowaniu. Przejęty członek może posiadać wszystkie swoje klucze, kłamać o danych/modelu, podpisywać sprzeczności, milczeć i generować ruch. Symulator oddzielnie modeluje radiowe zakłócanie. Nie obiecujemy ochrony przed fizycznym jammingiem ani ukrycia samego faktu transmisji.

## 9. Store-and-forward, przeciążenie i restarty

Węzły przechowują i przekazują cudze, niewygasłe komunikaty, zachowując pierwotną tożsamość i podpis. Relay nie jest dodatkowym głosem. Obowiązują deduplikacja, limity kolejek, przeskoków, retransmisji i ruchu na nadawcę. Priorytety zapewniają udział kanału mniej pilnym wiadomościom; oznaczenie „pilne” nie omija limitów. Nie można kryptograficznie zagwarantować, że przejęty przekaźnik uczciwie zmniejsza licznik przeskoków — bezpieczeństwo zasobów uczciwego węzła opiera się także na lokalnych limitach i deduplikacji.

Trwały stan w lokalnym SQLite obejmuje tożsamość kontekstu misji, rezerwacje liczników, historię własnych podpisanych decyzji, aktywne sprawy, odebrane głosy, etapy i niewygasły outbox. Klucze prywatne są przechowywane oddzielnie. Broker nie jest dziennikiem bezpieczeństwa. Po awarii trwałość oraz idempotencja muszą zapobiegać ponownemu logicznemu głosowi, również jeśli dostawa MQTT nastąpiła wielokrotnie.

Brak lokalnego ostrzeżenia: ograniczona kolejka oczekująca, bez własnego głosu do uzyskania danych. Niezgodny hash tej samej wersji: oddzielne zapisy konfliktu, bez łączenia poparcia. Cudza sprzeczna deklaracja nie stanowi globalnego polecenia wyłączenia głosowania wszystkich jednostek. Brak dozwolonego profilu/manifestu: wiadomość nie zwiększa poparcia. Po wygaśnięciu: tylko archiwum.

## 10. Symulacja i kryteria odbioru

Rdzeń ma wymienny zegar. Szybki symulator zdarzeniowy używa czasu wirtualnego i utrwalonego ziarna losowania. Próby integracyjne z procesami/kontenerami, MQTT i później modelami działają w czasie rzeczywistym. Logika walidacji i progów jest wspólna. Symulator ma wiedzę o środowisku, której nie ujawnia węzłom.

Model kanału musi uwzględniać airtime zależny od parametrów PHY i rozmiaru ramki, half-duplex, wspólny kanał i nakładające się transmisje, straty seryjne, topologię/asymetrię, partycje, opóźnione spotkania i ograniczenia czasu nadawania. Stopień uproszczenia propagacji, kolizji i capture effect opisujemy w każdym raporcie. Wynik nie jest pomiarem zasięgu na morzu.

Scenariusze: N=5,10,20; jedno ostrzeżenie co 10 minut; osobno pięć równoczesnych ostrzeżeń; heterogeniczne wagi; utrata najmocniejszych węzłów; partycje i ponowne połączenie; zegary przy granicy ważności; restart podczas zapisu/nadawania; brak lub konflikt danych; utrata uzupełnień; replay i przejęcia.

Kryteria twarde, sprawdzane na każdym uczciwym węźle w zdefiniowanych próbach:

1. Żadne zatwierdzenie nie występuje bez obu progów tego samego zbioru poprawnych głosów.
2. Obcy nadawca, niepoprawny podpis i niedozwolony profil nie zwiększają poparcia.
3. Replay, retransmisja, ponowiona dostawa lokalna i restart nie tworzą dodatkowego głosu.
4. Różne wersje, treści i etapy nie są sumowane.
5. Przekazanie i restart nie przedłużają ważności.
6. Znany powielony dowód nie zwiększa liczby źródeł pierwotnych.
7. Brak warunków do zatwierdzenia daje stan niepotwierdzony, nie automatyczny sukces.
8. Pamięć, kolejki i retransmisje mają egzekwowane limity.
9. Kontrolna próba bez napastników i z dostateczną łącznością oraz zgodnym poparciem kończy się zatwierdzeniem; implementacja zawsze odmawiająca nie zalicza testów.

Mierzymy osobno: czas decyzji, odsetek decyzji przed wygaśnięciem, airtime według typu wiadomości, retransmisje, rozmiary kolejek, RAM/CPU/dysk w integracji, błędne oceny modeli i fałszywe deklaracje niezależności. Cel 10 minut jest hipotezą do oceny, nie gwarancją dla każdego profilu radia. Testy nie stanowią formalnego dowodu bezpieczeństwa.

## 11. Integracja z istniejącym Morsikiem

Odczyt repo `Code/Baltic_Hackaton_26` wykazał:

- `src/domain/models/events.py`: `Event.id` jest domyślnie losowym UUID; nie jest wspólnym identyfikatorem sprawy flotowej.
- `src/services/slm/pipeline.py`: zapisuje model/backend/prompt/fallback i SHA-256 dokładnego tekstu wejściowego; ten hash nie utożsamia tłumaczeń i parafraz.
- `src/domain/correlation.py`: cluster ID zależy od lokalnych event ID, nie nadaje się bezpośrednio na klucz głosowania.
- `src/domain/assess.py` i `src/config/weights.yaml`: niezależność jest heurystyką klas źródeł, nie pełnym grafem pochodzenia; duplikaty tekstu są oznaczane osobnym caveat.
- `src/domain/schema.py`: model ekstrahuje dane, a obecny scoring jest osobny i deterministyczny; głosowanie i konsultacja będą nowym kontraktem, nie reinterpretacją `reliability.score`.

`morsik-lora` mapuje te dane na kontrakt rdzenia. Uzupełnienie wspólnego ID/rewizji, kanonicznej treści i pochodzenia dowodów wymaga osobnej integracji; nie twierdzimy, że obecny kod dostarcza gotową całość. Warstwa HTTP/MQTT i morskie kody przesłanek należą do aplikacji, nie do rdzenia `lcq`.

## 12. Proponowane doprecyzowania do przeglądu

Poniższe wybory nie zostały osobno zatwierdzone w rozmowie. Proponuję przyjąć je jako kierunek planu, a dokładne rozmiary i schemat binarny zatwierdzić przed implementacją kodeka:

- Pierwsza implementacja rdzenia i symulatora w Pythonie, z typowanymi kontraktami i testami właściwości; język docelowego firmware nie jest przez to narzucony.
- Wagi jako manifestowe liczby całkowite wyliczone offline. Dla pierwszego scenariusza referencyjnego modele mają ten sam profil kwantyzacji (`q=1`), następne scenariusze badają jawną syntetyczną tabelę `q` bez nazywania jej zmierzoną jakością. MoE dopuszczamy dopiero z jednoznaczną polityką aktywnych/całkowitych parametrów; początkowe profile są dense.
- Realizacja limitu 3:1 przez przycięcie surowej wagi do trzykrotności najmniejszej zatwierdzonej wagi surowej, następnie kontrolowane przeliczenie na liczby całkowite i ponowną walidację proporcji.
- Indywidualne podpisy Ed25519 i szyfrowanie AEAD ChaCha20-Poly1305. Osobny przegląd określi konstrukcję nonce, rozdzielenie kluczy/kontekstów, serializację i kolejność podpisywania/szyfrowania. Żaden z tych szczegółów nie może zostać pominięty jako „sprawa biblioteki”.
- Stan najpierw trwale rezerwuje tożsamość wysyłanej wiadomości i wiążący głos, dopiero potem dopuszcza transmisję. Retransmisja wysyła identyczny zapisany pakiet; utrata/rollback stanu bezpieczeństwa wymaga nowego kontekstu kluczy, nie resetu licznika pod starym kluczem.
- Zakończenie pojedynczej konsultacji ma stabilny identyfikator żądania; po awarii adapter może ponowić dostarczenie, ale aplikacja nie tworzy drugiej logicznej konsultacji ani nowego głosu.
- Zmienna liczba przesłanek nie zmienia przedmiotu wiążącego poparcia. Głos zatwierdza identyczny opis sprawy, a nie pełen lokalny zestaw obserwacji; uzupełnienia mają osobne referencje i podpisy.

Przed planem implementacji potrzebna jest akceptacja tego dokumentu. Plan ma zawierać tabelę dokładnych pól i rozmiarów, profil symulowanego PHY, limity zasobów oraz scenariusze testowe z oczekiwanymi wynikami. Nie należy traktować braku tych niskopoziomowych decyzji jako zgody na dowolny kodek lub konfigurację radia.
