# fnostudyqr-rs

A CLI query/retrieve tool for a bulk of DICOM studies.

## Usage

`fnostudyqr [OPTIONS] <ADDRESS> <COMMAND>`

* `-i, --in-study-file <PATH>`: File path input list of studies to query/retrieve. If not given the tool fallbacks to query tags.
* `-q, --query-tag <QUERY_TAG>`: Additional sequence of tags added to list of studies. Overwrites dataset tags from file if the tag has no value.
* `-l, --information-level`: Information level to request at, one of `study (default)`, `patient` or `series`. Restricts at what level tags/objects can be queried/retrieved.
* `-t, --calling-ae-title <AE>`: Caller application entity title.
* `-c, --called-ae-title <AE>`: Called application entity title.

**IMPORTANT!!** Query objects require non-empty matching identifier(s): Either `PatientID` with `StudyDate` or `StudyInstanceUID`, see table below:
| PatientID | StudyDate | StudyInstanceUID | Result |
| --------------- | --------------- | --------------- | --------------- |
| non-empty | non-empty | absent/empty/value | pass |
| absent/empty | absent/empty | value | pass |
| value | absent/empty | absent/empty | **fail** |
| absent/empty | value | absent/empty | **fail** |
| absent/empty | absent/empty | absent/empty | **fail** |



##### `find`-only options

* `-f, --out-response-path <PATH>`: File path to write response tags. If not given no responses are written.

##### Find example

`fnostudyqr remote@address:port -i <STUDIES_FILE> --calling-ae-title <AE> -q StudyDescription find -o study_tags.csv`

##### `move`-only options

* `--move-destination <AE>`: Destination application entity title. Defaults to `--calling-ae-title` if destination is caller
* `-p, --store-port <PORT>`: Store port to listen on if destination is caller.
* `-o, --output-dir <PATH>`: Output directory for incoming objects. Defaults to `./output`.

##### Move example

`fnostudyqr remote@address:port -i <STUDIES_FILE> --calling-ae-title <AE> move -p <STORE_PORT> -o ./download/`

---

##### Input file specifications

* Required file format is `.csv` with semicolon `;` separator or `.json`. Specify tags as keyword or hex value `(gggg,eeee)` with leading zeroes, ex. `PatientID`, `(0010,0020)` or `0010,0020`.
* Find request may contain any DICOM tags up to the requested information level, eg.: requesting PatientID and StudyDate at level `series` will match to and return all series per requested study. Value matching is case-sensitive.
* `TagKey=value` from terminal will be added to all query DICOM objects if not already present. Present tag with empty value (eg. `Modality=""` or `Modality=`) is not overwritten. Empty values can be empty strings `"", null, nan` (case-insensitive).
* Format of date elements **must be** `YYYY-MM-DD` and time elements `H:M:S`, including leading zeroes, otherwise values will be incorrectly matched.
* Per object query retrieval supported only for **JSON** file format and **find** command.

##### Example input files

```csv
PatientID;(0008,0020);StudyInstanceUID
01;1.2.3.4.5;
02;6.7.8.9.1;
```

```jsonc
[
  {
    "QueryRetrieveLevel": "STUDY", // per object query level supported
    "StudyInstanceUID": "1.2.3.4.5",
    "Modality": "" // empty value
  },
  {
    "StudyInstanceUID": "6.7.8.9.1",
    "SeriesNumber": "1\\2\\3", // study filtered by any of these
    "QueryRetrieveLevel": "SERIES"
  }
]
```

##### Example output file

```csv
PatientID;StudyInstanceUID;StudyDescription;...
01;1.2.3.4.5;AbdomenLungRoutine
02;6.7.8.9.1;HeadCT
```

```json
[
  {
    "StudyInstanceUID": "1.2.3.4.5",
    "Modality": "CT"
  },
  {
    "StudyInstanceUID": "6.7.8.9.1",
    "SeriesNumber": 1
  },
  {
    "StudyInstanceUID": "6.7.8.9.1",
    "SeriesNumber": 2
  },
  {
    "StudyInstanceUID": "6.7.8.9.1",
    "SeriesNumber": 3
  }
]
```

* Output tags sorted in ascending order.

##### Value matching

* Pattern matching supported with asterisk `(*)` for both input file and terminal.
* Separate date and time values (terminal or input file) allow for range matching. Use double-period `..` to specify date or time range. For example `-q StudyDate=2000-01-01..` (YYYY-MM-DD format) will match all studies since 1st January 2000.

## Acknowledgement

This terminal tool uses [dicom-rs's](https://github.com/Enet4/dicom-rs) *findscu* and *movescu* crates, combining them as one application. This repository only adds/changes some parts to allow querying/requesting a list of studies within a single runtime.
