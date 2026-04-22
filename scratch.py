import os
import re

def replace_style(content):
    # Replace Style::default() with ratatui::style::Style::new()
    content = content.replace("Style::default()", "ratatui::style::Style::new()")
    
    # Simple regex for Span::styled. For nested parens, we need a small parser.
    out = ""
    i = 0
    while i < len(content):
        idx = content.find("Span::styled(", i)
        if idx == -1:
            out += content[i:]
            break
        out += content[i:idx]
        
        # parse args of Span::styled(arg1, arg2)
        j = idx + len("Span::styled(")
        depth = 1
        start_arg1 = j
        arg1 = ""
        while j < len(content):
            if content[j] == '(': depth += 1
            elif content[j] == ')': depth -= 1
            elif content[j] == ',' and depth == 1:
                arg1 = content[start_arg1:j].strip()
                start_arg2 = j + 1
                break
            j += 1
        
        j = start_arg2
        depth = 1
        arg2 = ""
        while j < len(content):
            if content[j] == '(': depth += 1
            elif content[j] == ')':
                depth -= 1
                if depth == 0:
                    arg2 = content[start_arg2:j].strip()
                    break
            j += 1
            
        # construct replacement: (arg1).set_style(arg2)
        out += f"({arg1}).set_style({arg2})"
        i = j + 1

    return out

for root, _, files in os.walk("crates/crow-cli/src/tui"):
    for file in files:
        if file.endswith(".rs"):
            path = os.path.join(root, file)
            with open(path, "r") as f:
                content = f.read()
            new_content = replace_style(content)
            # Add missing import if set_style is used
            if ".set_style(" in new_content and "use ratatui::style::Styled;" not in new_content:
                new_content = "use ratatui::style::Styled;\n" + new_content
            
            if new_content != content:
                with open(path, "w") as f:
                    f.write(new_content)
