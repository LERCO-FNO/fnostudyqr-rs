pub fn format_date(date_str: &str) -> String {
    date_str.split('-').collect::<Vec<&str>>().join("")
}

pub fn format_date_range(date_str: &str) -> String {
    date_str
        .split("..")
        .map(format_date)
        .collect::<Vec<String>>()
        .join("-")
}

pub fn format_time(time_str: &str) -> String {
    time_str.split(':').collect::<Vec<&str>>().join("")
}

pub fn format_time_range(time_str: &str) -> String {
    time_str
        .split("..")
        .map(format_time)
        .collect::<Vec<String>>()
        .join("-")
}
