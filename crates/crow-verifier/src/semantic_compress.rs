pub fn compress_rust_logs(raw: &str) -> String {
    let mut condensed = String::new();
    let lines: Vec<&str> = raw.lines().collect();
    
    if lines.is_empty() {
        return raw.to_string();
    }
    
    let mut capture_context = false;
    let mut captured_lines = 0;
    
    // We always keep the first 5 lines for context
    for line in lines.iter().take(5) {
        condensed.push_str(line);
        condensed.push('\n');
    }
    condensed.push_str("... (semantic truncation) ...\n");
    
    for (i, &line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        
        // Fast paths to ignore noisy cargo output
        if trimmed.starts_with("Compiling ") 
            || trimmed.starts_with("Downloaded ")
            || trimmed.starts_with("Downloading ")
            || trimmed.starts_with("Blocking waiting for file lock")
            || trimmed.starts_with("Finished ") {
               continue;
        }
        
        let is_rustc_error = trimmed.starts_with("error:") || trimmed.starts_with("error[E");
        let is_panic = trimmed.starts_with("thread ") && trimmed.contains("panicked at");
        let is_test_failure = trimmed.starts_with("failures:") && i > 0;
        
        if is_rustc_error || is_panic || is_test_failure {
            capture_context = true;
            captured_lines = 0;
            condensed.push_str("\n--- ⚡ Critical Error Context ---\n");
        }
        
        if capture_context {
            condensed.push_str(line);
            condensed.push('\n');
            captured_lines += 1;
            
            // Capture up to 40 lines of context per error to grab the stack trace and code snippet
            if captured_lines > 40 {
                capture_context = false;
                condensed.push_str("... (error block truncated) ...\n");
            }
            
            // Stop capturing if we hit an empty line after a reasonable chunk
            if trimmed.is_empty() && captured_lines > 5 {
                capture_context = false;
            }
        }
    }
    
    // We always keep the last 5 lines (e.g. final test result summary)
    condensed.push_str("\n... (semantic truncation) ...\n");
    let total_len = lines.len();
    for line in lines.iter().skip(total_len.saturating_sub(5)) {
        condensed.push_str(line);
        condensed.push('\n');
    }
    
    // If the semantic compression didn't actually reduce the length significantly,
    // just return the original string to avoid missing things.
    if condensed.len() >= raw.len() / 2 && raw.len() < 10000 {
        return raw.to_string();
    }
    
    condensed
}
