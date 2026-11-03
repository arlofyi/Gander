### 01-files-listing: Files app browses the root (queries only, no reads)

Log: `01-files-listing.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |


### 02-files-open-faststart-try1: Files app tap on sample-faststart.mp4 (attempt 1), handed to the system video player

Log: `02-files-open-faststart-try1.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | com.google.android.apps.photos | sample-faststart.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 2 | com.google.android.apps.photos | sample-faststart.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 3 | com.google.android.apps.photos | sample-faststart.mp4 | 33 | 904.0 KiB | 904.0 KiB | 0% | 9 | 23 | 1 | 4.0 KiB/8.0 KiB/128.0 KiB | 1.0/3.1/3.1 | ? |

**fd 3** (com.google.android.apps.photos, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 4096 x9, 8192 x8, 24576 x3, 36864 x3, 131072 x3, 12288 x2, 32768 x2, 16384 x1
- offsets: first read at 0, last read ends at 638976, unique coverage 3.0%
- forward gaps (bytes skipped): median 24.0 KiB, p90 36.0 KiB, max 36.0 KiB
- callback threads: spike-proxy-fd


Top activity after 16 s: `topResumedActivity=ActivityRecord{158945545 u0 com.google.android.apps.photos/.pager.HostPhotoPagerActivity t23}`. NuPlayer: `state(4), atEOS(0), looping(0), autoLoop(0), renderer(null)` (state 4 = prepared, playback not started; state 5 = running).


### 03-files-open-moovend-try1: Files app tap on sample-moovend.mp4 (attempt 1), handed to the system video player

Log: `03-files-open-moovend-try1.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | com.google.android.apps.photos | sample-moovend.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 2 | com.google.android.apps.photos | sample-moovend.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 3 | com.google.android.apps.photos | sample-moovend.mp4 | 1280 | 17.17 MiB | 17.17 MiB | 0% | 100 | 1170 | 10 | 4.0 KiB/8.0 KiB/128.0 KiB | 1.7/6.7/34.0 | ? |

**fd 3** (com.google.android.apps.photos, sample-moovend.mp4, file size 30811338)
- requested sizes (top 8 by count): 4096 x354, 8192 x350, 20480 x223, 16384 x192, 12288 x67, 24576 x29, 28672 x24, 131072 x16
- offsets: first read at 0, last read ends at 17838080, unique coverage 58.4%
- forward gaps (bytes skipped): median 8.0 KiB, p90 20.0 KiB, max 28.99 MiB
- jumps of at least 1.00 MiB (10):
  - read #5 forward 28.99 MiB: 376832 -> 30773248
  - read #8 backward 28.99 MiB: 30811338 -> 409600
  - read #71 backward 1.59 MiB: 2310144 -> 638976
  - read #203 backward 1.89 MiB: 4710400 -> 2732032
  - read #370 backward 1.91 MiB: 6963200 -> 4964352
  - read #544 backward 2.11 MiB: 9269248 -> 7061504
  - read #732 backward 2.13 MiB: 11522048 -> 9289728
  - read #909 backward 1.80 MiB: 13799424 -> 11911168
  - read #1054 backward 1.33 MiB: 16056320 -> 14663680
  - read #1201 backward 2.13 MiB: 18345984 -> 16109568
- callback threads: spike-proxy-fd


Top activity after 16 s: `topResumedActivity=ActivityRecord{220752369 u0 com.google.android.apps.photos/.pager.HostPhotoPagerActivity t24}`. NuPlayer: `state(5), atEOS(0), looping(0), autoLoop(0), renderer(paused(0), offloading(0), wakelock(acquired=[0,0,0], timeout=[0,0,0], release=[0,0,0], cancel=[0,0,0]))` (state 4 = prepared, playback not started; state 5 = running).


### 04-files-open-faststart-try2: Files app tap on sample-faststart.mp4 (attempt 2), handed to the system video player

Log: `04-files-open-faststart-try2.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | com.google.android.apps.photos | sample-faststart.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 2 | com.google.android.apps.photos | sample-faststart.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 3 | com.google.android.apps.photos | sample-faststart.mp4 | 1234 | 16.16 MiB | 16.16 MiB | 0% | 94 | 1131 | 9 | 4.0 KiB/8.0 KiB/128.0 KiB | 1.1/4.0/81.2 | ? |

**fd 3** (com.google.android.apps.photos, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 4096 x348, 8192 x325, 20480 x203, 16384 x193, 12288 x72, 24576 x39, 131072 x15, 28672 x15
- offsets: first read at 0, last read ends at 16801792, unique coverage 55.0%
- forward gaps (bytes skipped): median 8.0 KiB, p90 20.0 KiB, max 524.0 KiB
- jumps of at least 1.00 MiB (7):
  - read #69 backward 1.62 MiB: 2342912 -> 638976
  - read #202 backward 1.89 MiB: 4747264 -> 2768896
  - read #376 backward 2.16 MiB: 7000064 -> 4734976
  - read #563 backward 2.11 MiB: 9306112 -> 7098368
  - read #836 backward 1.87 MiB: 12726272 -> 10768384
  - read #1006 backward 1.86 MiB: 14950400 -> 12996608
  - read #1171 backward 1.82 MiB: 17272832 -> 15360000
- callback threads: spike-proxy-fd


Top activity after 16 s: `topResumedActivity=ActivityRecord{223949946 u0 com.google.android.apps.photos/.pager.HostPhotoPagerActivity t25}`. NuPlayer: `state(5), atEOS(0), looping(0), autoLoop(0), renderer(paused(0), offloading(0), wakelock(acquired=[0,0,0], timeout=[0,0,0], release=[0,0,0], cancel=[0,0,0]))` (state 4 = prepared, playback not started; state 5 = running).


### 05-files-open-moovend-try2: Files app tap on sample-moovend.mp4 (attempt 2), handed to the system video player

Log: `05-files-open-moovend-try2.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | com.google.android.apps.photos | sample-moovend.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 2 | com.google.android.apps.photos | sample-moovend.mp4 | 0 | 0 | 0 | - | - | - | - | - | - | - |
| 3 | com.google.android.apps.photos | sample-moovend.mp4 | 1253 | 16.79 MiB | 16.79 MiB | 0% | 96 | 1147 | 10 | 4.0 KiB/8.0 KiB/128.0 KiB | 1.2/3.8/32.2 | ? |

**fd 3** (com.google.android.apps.photos, sample-moovend.mp4, file size 30811338)
- requested sizes (top 8 by count): 4096 x352, 8192 x350, 20480 x216, 16384 x174, 12288 x64, 24576 x32, 28672 x24, 131072 x17
- offsets: first read at 0, last read ends at 17289216, unique coverage 57.1%
- forward gaps (bytes skipped): median 8.0 KiB, p90 20.0 KiB, max 28.99 MiB
- jumps of at least 1.00 MiB (10):
  - read #5 forward 28.99 MiB: 376832 -> 30773248
  - read #8 backward 28.99 MiB: 30811338 -> 409600
  - read #71 backward 1.59 MiB: 2310144 -> 638976
  - read #203 backward 1.89 MiB: 4710400 -> 2732032
  - read #377 backward 2.16 MiB: 6963200 -> 4702208
  - read #564 backward 2.11 MiB: 9269248 -> 7061504
  - read #744 backward 1.88 MiB: 11522048 -> 9551872
  - read #909 backward 1.80 MiB: 13799424 -> 11911168
  - read #1054 backward 1.33 MiB: 16056320 -> 14663680
  - read #1201 backward 2.13 MiB: 18345984 -> 16109568
- callback threads: spike-proxy-fd


Top activity after 16 s: `topResumedActivity=ActivityRecord{241556456 u0 com.google.android.apps.photos/.pager.HostPhotoPagerActivity t26}`. NuPlayer: `state(5), atEOS(0), looping(0), autoLoop(0), renderer(paused(0), offloading(0), wakelock(acquired=[0,0,0], timeout=[0,0,0], release=[0,0,0], cancel=[0,0,0]))` (state 4 = prepared, playback not started; state 5 = running).


### 06-files-copy-faststart: Files app "Copy to... Downloads" (a full copy by DocumentsUI)

Log: `06-files-copy-faststart.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | com.google.android.documentsui | sample-faststart.mp4 | 236 | 29.38 MiB | 29.38 MiB | 0% | 236 | 0 | 0 | 12.0 KiB/128.0 KiB/128.0 KiB | 1.3/4.2/11.8 | 236 |

**fd 1** (com.google.android.documentsui, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x235, 12288 x1
- offsets: first read at 0, last read ends at 30811338, unique coverage 100.0%
- callback threads: spike-proxy-fd
- release: http_bytes=30811338 http_requests=236 max_in_flight=1 life_ms=764


Copy finished: True. Size of /sdcard/Download/sample-faststart.mp4 after the copy: 30811338 bytes (source 30811338).


### 07-files-getinfo-faststart: Files app "Get info"

Log: `07-files-getinfo-faststart.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |


Get info tapped: True.


### 08-player-sample-faststart: Platform MediaPlayer (VideoView), sample-faststart.mp4, scripted seeks to 50 %, 90 %, 10 %

Log: `08-player-sample-faststart.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 1043 | 13.79 MiB | 13.79 MiB | 0% | 66 | 967 | 10 | 4.0 KiB/8.0 KiB/128.0 KiB | 1.0/4.7/72.1 | 1043 |

**fd 1** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 4096 x321, 8192 x317, 16384 x144, 20480 x121, 12288 x42, 24576 x38, 28672 x18, 131072 x17
- offsets: first read at 0, last read ends at 8818688, unique coverage 46.9%
- forward gaps (bytes skipped): median 16.0 KiB, p90 20.0 KiB, max 10.46 MiB
- jumps of at least 1.00 MiB (10):
  - read #69 backward 1.62 MiB: 2342912 -> 638976
  - read #164 backward 1.06 MiB: 3616768 -> 2506752
  - read #291 backward 1.91 MiB: 5820416 -> 3817472
  - read #309 forward 10.46 MiB: 4206592 -> 15171584
  - read #461 backward 1.86 MiB: 18694144 -> 16748544
  - read #624 forward 6.00 MiB: 20946944 -> 27238400
  - read #770 backward 1.98 MiB: 30765056 -> 28684288
  - read #827 backward 24.59 MiB: 29995008 -> 4210688
  - read #832 forward 1.43 MiB: 4341760 -> 5836800
  - read #861 backward 2.12 MiB: 6569984 -> 4341760
- callback threads: spike-proxy-fd
- release: http_bytes=14454784 http_requests=1043 max_in_flight=1 life_ms=17103

**Seek behaviour** (reads the provider saw between each seek request and the next seek or the end)
- seek to 20000 ms (from 4511 ms): done after 186 ms with 36 reads; first jump of 10.46 MiB (forward) lands at offset 15171584 after 0 reads; first reads (offset/size) 15171584/69632, 15241216/65536, 15306752/131072, 15437824/131072; 249 reads and 3.70 MiB until the next seek
- seek to 36000 ms (from 23418 ms): done after 351 ms with 99 reads; first jump of 6.00 MiB (forward) lands at offset 27238400 after 66 reads; first reads (offset/size) 19263488/8192, 19288064/4096, 19312640/4096, 19333120/8192; 269 reads and 3.20 MiB until the next seek
- seek to 4000 ms (from 38080 ms): done after 146 ms with 0 reads; first jump of 24.59 MiB (backward) lands at offset 4210688 after 0 reads; first reads (offset/size) 4210688/24576, 4239360/24576, 4268032/20480, 4296704/20480; 134 reads and 2.05 MiB until the next seek


### 09-player-sample-moovend: Platform MediaPlayer (VideoView), sample-moovend.mp4, scripted seeks to 50 %, 90 %, 10 %

Log: `09-player-sample-moovend.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | fyi.arlo.gander.spike.files | sample-moovend.mp4 | 993 | 13.48 MiB | 13.48 MiB | 0% | 68 | 914 | 11 | 4.0 KiB/8.0 KiB/128.0 KiB | 1.7/4.8/71.3 | 993 |

**fd 1** (fyi.arlo.gander.spike.files, sample-moovend.mp4, file size 30811338)
- requested sizes (top 8 by count): 4096 x301, 8192 x272, 20480 x156, 16384 x130, 12288 x45, 24576 x31, 28672 x22, 131072 x16
- offsets: first read at 0, last read ends at 7061504, unique coverage 45.9%
- forward gaps (bytes skipped): median 12.0 KiB, p90 20.0 KiB, max 28.99 MiB
- jumps of at least 1.00 MiB (11):
  - read #5 forward 28.99 MiB: 376832 -> 30773248
  - read #8 backward 28.99 MiB: 30811338 -> 409600
  - read #71 backward 1.59 MiB: 2310144 -> 638976
  - read #206 backward 2.01 MiB: 4710400 -> 2600960
  - read #273 forward 10.58 MiB: 4042752 -> 15134720
  - read #425 backward 1.86 MiB: 18661376 -> 16711680
  - read #509 forward 8.00 MiB: 18808832 -> 27201536
  - read #658 backward 2.11 MiB: 30728192 -> 28516352
  - read #722 backward 24.71 MiB: 29958144 -> 4046848
  - read #806 backward 2.11 MiB: 6533120 -> 4317184
  - read #983 backward 1.89 MiB: 8781824 -> 6799360
- callback threads: spike-proxy-fd
- release: http_bytes=14136522 http_requests=993 max_in_flight=1 life_ms=17027

**Seek behaviour** (reads the provider saw between each seek request and the next seek or the end)
- seek to 20000 ms (from 4168 ms): done after 315 ms with 36 reads; first jump of 10.58 MiB (forward) lands at offset 15134720 after 0 reads; first reads (offset/size) 15134720/69632, 15204352/65536, 15269888/131072, 15400960/131072; 236 reads and 3.50 MiB until the next seek
- seek to 36000 ms (from 23264 ms): done after 366 ms with 33 reads; first jump of 8.00 MiB (forward) lands at offset 27201536 after 0 reads; first reads (offset/size) 27201536/69632, 27271168/65536, 27336704/131072, 27467776/131072; 213 reads and 2.80 MiB until the next seek
- seek to 4000 ms (from 38102 ms): done after 185 ms with 0 reads; first jump of 24.71 MiB (backward) lands at offset 4046848 after 0 reads; first reads (offset/size) 4046848/20480, 4071424/20480, 4096000/20480, 4124672/20480; 254 reads and 2.90 MiB until the next seek


### 10-player-faststart-readahead1m: Platform MediaPlayer, sample-faststart.mp4, provider read-ahead 1 MiB

Log: `10-player-faststart-readahead1m.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 1128 | 14.56 MiB | 14.56 MiB | 0% | 71 | 1046 | 11 | 4.0 KiB/8.0 KiB/128.0 KiB | 0.0/0.1/137.4 | 36 |

**fd 1** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 8192 x335, 4096 x334, 20480 x157, 16384 x154, 12288 x53, 24576 x39, 28672 x18, 131072 x15
- offsets: first read at 0, last read ends at 7491584, unique coverage 49.6%
- forward gaps (bytes skipped): median 16.0 KiB, p90 20.0 KiB, max 10.46 MiB
- jumps of at least 1.00 MiB (10):
  - read #69 backward 1.62 MiB: 2342912 -> 638976
  - read #208 backward 2.14 MiB: 4747264 -> 2506752
  - read #284 forward 10.46 MiB: 4206592 -> 15171584
  - read #436 backward 1.86 MiB: 18694144 -> 16748544
  - read #599 forward 6.00 MiB: 20946944 -> 27238400
  - read #706 backward 1.14 MiB: 29614080 -> 28422144
  - read #804 backward 1.15 MiB: 30811338 -> 29601792
  - read #834 backward 24.84 MiB: 30257152 -> 4210688
  - read #913 backward 2.12 MiB: 6569984 -> 4341760
  - read #1095 backward 2.02 MiB: 8818688 -> 6705152
- callback threads: spike-proxy-fd
- release: http_bytes=36984010 http_requests=36 max_in_flight=1 life_ms=16753

**Seek behaviour** (reads the provider saw between each seek request and the next seek or the end)
- seek to 20000 ms (from 4471 ms): done after 169 ms with 36 reads; first jump of 10.46 MiB (forward) lands at offset 15171584 after 0 reads; first reads (offset/size) 15171584/69632, 15241216/65536, 15306752/131072, 15437824/131072; 248 reads and 3.69 MiB until the next seek
- seek to 36000 ms (from 23440 ms): done after 177 ms with 100 reads; first jump of 6.00 MiB (forward) lands at offset 27238400 after 67 reads; first reads (offset/size) 19238912/8192, 19263488/8192, 19288064/4096, 19312640/4096; 302 reads and 3.42 MiB until the next seek
- seek to 4000 ms (from 38271 ms): done after 168 ms with 0 reads; first jump of 24.84 MiB (backward) lands at offset 4210688 after 0 reads; first reads (offset/size) 4210688/24576, 4239360/24576, 4268032/20480, 4296704/20480; 261 reads and 2.79 MiB until the next seek


### 11-probe-seq: Sequential client reads, buffers 4 KiB, 64 KiB, 1 MiB, 8 MiB (one fd each, in this order)

Log: `11-probe-seq.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 1 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 238 | 29.38 MiB | 29.38 MiB | 0% | 238 | 0 | 0 | 16.0 KiB/128.0 KiB/128.0 KiB | 3.4/8.9/26.1 | 238 |
| 2 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 236 | 29.38 MiB | 29.38 MiB | 0% | 236 | 0 | 0 | 12.0 KiB/128.0 KiB/128.0 KiB | 1.4/3.0/4.0 | 236 |
| 3 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 236 | 29.38 MiB | 29.38 MiB | 0% | 236 | 0 | 0 | 12.0 KiB/128.0 KiB/128.0 KiB | 1.2/2.8/7.0 | 236 |
| 4 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 236 | 29.38 MiB | 29.38 MiB | 0% | 236 | 0 | 0 | 12.0 KiB/128.0 KiB/128.0 KiB | 1.4/2.4/6.0 | 236 |

**fd 1** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x234, 16384 x1, 28672 x1, 32768 x1, 65536 x1
- offsets: first read at 0, last read ends at 30811338, unique coverage 100.0%
- callback threads: spike-proxy-fd
- release: http_bytes=30811338 http_requests=238 max_in_flight=1 life_ms=1403

**fd 2** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x235, 12288 x1
- offsets: first read at 0, last read ends at 30811338, unique coverage 100.0%
- callback threads: spike-proxy-fd
- release: http_bytes=30811338 http_requests=236 max_in_flight=1 life_ms=570

**fd 3** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x235, 12288 x1
- offsets: first read at 0, last read ends at 30811338, unique coverage 100.0%
- callback threads: spike-proxy-fd
- release: http_bytes=30811338 http_requests=236 max_in_flight=1 life_ms=459

**fd 4** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x235, 12288 x1
- offsets: first read at 0, last read ends at 30811338, unique coverage 100.0%
- callback threads: spike-proxy-fd
- release: http_bytes=30811338 http_requests=236 max_in_flight=1 life_ms=563


### 12-probe-rand: Random client preads: 200 x 4 KiB, 100 x 64 KiB, 50 x 1 MiB (one fd each, in this order)

Log: `12-probe-rand.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 5 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 198 | 1.54 MiB | 1.54 MiB | 0% | 1 | 96 | 101 | 4.0 KiB/8.0 KiB/12.0 KiB | 1.4/7.2/17.7 | 198 |
| 6 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 99 | 6.34 MiB | 6.34 MiB | 0% | 1 | 47 | 51 | 4.0 KiB/68.0 KiB/128.0 KiB | 1.2/4.1/9.7 | 99 |
| 7 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 200 | 23.88 MiB | 23.88 MiB | 0% | 168 | 15 | 17 | 4.0 KiB/128.0 KiB/128.0 KiB | 1.1/2.4/4.0 | 200 |

**fd 5** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 8192 x192, 4096 x4, 12288 x2
- offsets: first read at 22413312, last read ends at 2306048, unique coverage 5.2%
- forward gaps (bytes skipped): median 8.03 MiB, p90 20.05 MiB, max 24.63 MiB
- jumps of at least 1.00 MiB (180):
  - read #1 backward 1.31 MiB: 22421504 -> 21045248
  - read #2 backward 11.01 MiB: 21053440 -> 9506816
  - read #4 forward 11.41 MiB: 8540160 -> 20500480
  - read #5 forward 6.98 MiB: 20508672 -> 27828224
  - read #6 backward 15.71 MiB: 27836416 -> 11358208
  - read #7 backward 2.74 MiB: 11366400 -> 8491008
  - read #8 forward 5.52 MiB: 8499200 -> 14282752
  - read #9 forward 9.37 MiB: 14290944 -> 24117248
  - read #10 forward 4.00 MiB: 24125440 -> 28319744
  - read #11 backward 14.20 MiB: 28327936 -> 13443072
  - read #12 forward 9.20 MiB: 13451264 -> 23101440
  - read #13 backward 10.68 MiB: 23109632 -> 11907072
- callback threads: spike-proxy-fd
- release: http_bytes=1613824 http_requests=198 max_in_flight=1 life_ms=732

**fd 6** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 69632 x80, 40960 x3, 12288 x2, 57344 x2, 77824 x2, 4096 x1, 8192 x1, 16384 x1
- offsets: first read at 22368256, last read ends at 16924672, unique coverage 21.6%
- forward gaps (bytes skipped): median 9.12 MiB, p90 18.96 MiB, max 20.40 MiB
- jumps of at least 1.00 MiB (88):
  - read #1 backward 1.37 MiB: 22437888 -> 21004288
  - read #2 backward 11.05 MiB: 21073920 -> 9490432
  - read #4 forward 11.32 MiB: 8585216 -> 20459520
  - read #5 forward 6.91 MiB: 20529152 -> 27770880
  - read #6 backward 15.74 MiB: 27840512 -> 11337728
  - read #7 backward 2.80 MiB: 11407360 -> 8474624
  - read #8 forward 5.47 MiB: 8515584 -> 14254080
  - read #9 forward 9.29 MiB: 14323712 -> 24068096
  - read #10 forward 3.93 MiB: 24137728 -> 28262400
  - read #11 backward 14.22 MiB: 28332032 -> 13418496
  - read #12 forward 9.12 MiB: 13488128 -> 23056384
  - read #13 backward 10.72 MiB: 23126016 -> 11882496
- callback threads: spike-proxy-fd
- release: http_bytes=6651904 http_requests=99 max_in_flight=1 life_ms=281

**fd 7** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x186, 24576 x3, 49152 x2, 73728 x2, 4096 x1, 8192 x1, 20480 x1, 40960 x1
- offsets: first read at 21651456, last read ends at 15765504, unique coverage 81.3%
- forward gaps (bytes skipped): median 8.74 MiB, p90 16.31 MiB, max 19.83 MiB
- jumps of at least 1.00 MiB (31):
  - read #10 backward 2.51 MiB: 22962176 -> 20332544
  - read #20 backward 11.88 MiB: 21643264 -> 9187328
  - read #30 backward 2.15 MiB: 10498048 -> 8245248
  - read #38 forward 10.13 MiB: 9187328 -> 19808256
  - read #42 forward 6.25 MiB: 20332544 -> 26886144
  - read #52 backward 16.43 MiB: 28196864 -> 10973184
  - read #62 backward 3.89 MiB: 12283904 -> 8204288
  - read #63 forward 5.30 MiB: 8245248 -> 13799424
  - read #73 forward 7.81 MiB: 15110144 -> 23298048
  - read #83 forward 3.42 MiB: 24608768 -> 28196864
  - read #86 backward 14.88 MiB: 28590080 -> 12988416
  - read #93 forward 8.74 MiB: 13799424 -> 22962176
- callback threads: spike-proxy-fd
- release: http_bytes=25034752 http_requests=200 max_in_flight=1 life_ms=400


### 13-probe-seq-readahead1m: Sequential 1 MiB client reads, provider read-ahead 1 MiB

Log: `13-probe-seq-readahead1m.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 8 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 236 | 29.38 MiB | 29.38 MiB | 0% | 236 | 0 | 0 | 12.0 KiB/128.0 KiB/128.0 KiB | 0.0/7.1/13.6 | 30 |

**fd 8** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x235, 12288 x1
- offsets: first read at 0, last read ends at 30811338, unique coverage 100.0%
- callback threads: spike-proxy-fd
- release: http_bytes=30811338 http_requests=30 max_in_flight=1 life_ms=600


### 14-probe-parallel-shared: Two fds in parallel, handler=shared: t0 reads at 20,000,000 and is delayed 5 s by the server; t1 starts 1 s later and reads the first 1 MiB

Log: `14-probe-parallel-shared.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 9 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 2 | 196.0 KiB | 196.0 KiB | 0% | 2 | 0 | 0 | 68.0 KiB/68.0 KiB/128.0 KiB | 5,004.1/5,006.9/5,006.9 | 2 |
| 10 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 9 | 1.12 MiB | 1.12 MiB | 0% | 9 | 0 | 0 | 128.0 KiB/128.0 KiB/128.0 KiB | 2.8/11.2/11.2 | 9 |

**fd 9** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 69632 x1, 131072 x1
- offsets: first read at 19996672, last read ends at 20197376, unique coverage 0.7%
- callback threads: spike-proxy-fd
- release: http_bytes=200704 http_requests=2 max_in_flight=1 life_ms=10056

**fd 10** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x9
- offsets: first read at 0, last read ends at 1179648, unique coverage 3.8%
- callback threads: spike-proxy-fd
- release: http_bytes=1179648 http_requests=9 max_in_flight=1 life_ms=9114


### 15-probe-parallel-perfd: Two fds in parallel, handler=perfd: t0 reads at 20,000,000 and is delayed 5 s by the server; t1 starts 1 s later and reads the first 1 MiB

Log: `15-probe-parallel-perfd.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 11 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 2 | 196.0 KiB | 196.0 KiB | 0% | 2 | 0 | 0 | 68.0 KiB/68.0 KiB/128.0 KiB | 5,003.8/5,004.8/5,004.8 | 2 |
| 12 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 9 | 1.12 MiB | 1.12 MiB | 0% | 9 | 0 | 0 | 128.0 KiB/128.0 KiB/128.0 KiB | 2.6/7.5/7.5 | 9 |

**fd 11** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 69632 x1, 131072 x1
- offsets: first read at 19996672, last read ends at 20197376, unique coverage 0.7%
- callback threads: spike-proxy-fd-11
- release: http_bytes=200704 http_requests=2 max_in_flight=1 life_ms=10034

**fd 12** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 131072 x9
- offsets: first read at 0, last read ends at 1179648, unique coverage 3.8%
- callback threads: spike-proxy-fd-12
- release: http_bytes=1179648 http_requests=9 max_in_flight=1 life_ms=73


### 16-probe-slow-read-20s: One read delayed 20 s by the server

Log: `16-probe-slow-read-20s.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 13 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 2 | 196.0 KiB | 196.0 KiB | 0% | 2 | 0 | 0 | 68.0 KiB/68.0 KiB/128.0 KiB | 20,004.0/20,006.2/20,006.2 | 2 |

**fd 13** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 69632 x1, 131072 x1
- offsets: first read at 29999104, last read ends at 30199808, unique coverage 0.7%
- callback threads: spike-proxy-fd
- release: http_bytes=200704 http_requests=2 max_in_flight=1 life_ms=40046


### 17-probe-slow-read-75s: One read delayed 75 s by the server

Log: `17-probe-slow-read-75s.txt`

| fd | client | doc | reads | requested | unique | re-read | seq | fwd | back | sizes min/med/max | cb ms med/p95/max | HTTP reqs |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- | --- | ---: |
| 14 | fyi.arlo.gander.spike.files | sample-faststart.mp4 | 2 | 196.0 KiB | 196.0 KiB | 0% | 2 | 0 | 0 | 68.0 KiB/68.0 KiB/128.0 KiB | 75,002.7/75,003.3/75,003.3 | 2 |

**fd 14** (fyi.arlo.gander.spike.files, sample-faststart.mp4, file size 30811338)
- requested sizes (top 8 by count): 69632 x1, 131072 x1
- offsets: first read at 29999104, last read ends at 30199808, unique coverage 0.7%
- callback threads: spike-proxy-fd
- release: http_bytes=200704 http_requests=2 max_in_flight=1 life_ms=150064


