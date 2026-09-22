# fnostudyqr-rs

A CLI query/retrieve tool for a bulk of DICOM studies.

## Usage

`fnostudyqr [OPTIONS] <ADDRESS> <COMMAND>`

* `-i, --in-study-file <PATH>`: File path input list of studies to query/retrieve. If not given the tool fallbacks to query tags.
* `-t, --query-tag <QUERY_TAG>`: Additional sequence of tags added to list of studies. Overwrites dataset tags from file if the tag has no value.
* `-l, --information-level`: Information level to request at. One of `study (default)`, `patient` or `series`. Restricts at what level tags/objects can be queried/retrieved.
* `--calling-ae-title <AE>`: Caller application entity title.
* `--called-ae-title <AE>`: Called application entity title.

##### `find`-only options

* `-o, --out-study-file (-o) <PATH>`: File path with responses. Defaults to `responses.csv`.

##### Find example

`fnostudyqr remote@address:port -i <STUDIES_FILE> --calling-ae-title <AE> -t StudyDescription find -o study_tags.csv`

##### `move`-only options

* `--move-destination <AE>`: Destination application entity title. Must be equal to `--calling-ae-title` if destination is caller.
* `-p, --store-port (-p) <PORT>`: Store port to listen on if destination is caller.
* `-o, --output-dir (-o) <PORT>`: Output directory for incoming objects. Defaults to `./output`.

##### Move example

`fnostudyqr remote@address:port -i <STUDIES_FILE> --calling-ae-title <AE> move -p <STORE_PORT> -o ./download/`

---

##### Input file specifications

* Required file format is `.csv` with semicolon `;` separator.
Tags can be specified as keyword or hex value `(gggg,eeee)` with leading zeroes, ex. `PatientID`, `(0010,0020)` or `0010,0020`.
* Find request may contain any DICOM tags up to the requested information level, ex: requesting PatientID and StudyDate at level `series` will match to all series per requested study. Value matching is case-sensitive.
* Empty tag value will be overwritten by matching `<query-tag>` value.
* Order of date elements must be *YYYY-MM-DD* and time elements *H:M:S*, including leading zeroes, otherwise values will be incorrectly matched.

##### Example input file

```csv
PatientID;(0008,0020);StudyInstanceUID
01;20050101;
02;20050101;
```

##### Example output file

```csv
PatientID;StudyDate;StudyDescription;...
01;20050101;AbdomenLungRoutine
02;20050101;HeadCT
```

* File tags precede command line tags.

##### Value matching

* Tag values in input file and command line allow for pattern matching with asterisk `*`.
* Separate date and time values (command line or input file) allow for range matching. Use double-period `..` to specify date or time range. For example, `-t StudyDate=2000-01-01..` (YYYY-MM-DD) will match all studies since this date 1st January 2000.

## Acknowledgement

This command line tool uses [dicom-rs's](https://github.com/Enet4/dicom-rs) *findscu* and *movescu* crates, combining them as one application. This repository only adds/changes some parts to allow querying/requesting a list of studies within a single runtime.
