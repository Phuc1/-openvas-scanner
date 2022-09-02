#!/bin/bash

make_entry() {
    toc="$toc<li>"

    line=$(head -n 1 "$entry")
    title=${line:2}

    link=$entry
    link=${link//documentation/html}
    link=${link//.md/.html}
    
    toc="$toc<a href=$link>$title</a>"

    toc="$toc</li>"
}

recursive_toc() {
    for entry in "$search_dir"/*
    do
        toc="$toc<ul>"     
        # In case of folder iterate through it
        if [[ -d $entry ]]; then
            search_dir="$entry"
            entry="$entry"/index.md
            make_entry
            entry=${entry//\/index.md/""}
            recursive_toc
        # Else make an entry for the file
        elif [[ -f $entry ]]; then
            filename="$(basename -- $entry)"
            if [ $filename != index.md ]; then
                make_entry
            fi
        fi
        toc="$toc</ul>"
    done
}

create_html_dict() {
    dict=$entry
    dict=${dict//documentation/html}
    mkdir $dict
}

make_html() {
    content=$(pandoc -f markdown -t html $entry)
    content=${content//.md/.html}

    html=$template
    html=${html//\%CSS\%/${css_path}}
    html=${html//\%TOC\%/${toc}}
    html=${html//\%CONTENT\%/${content}}
    
    file=$entry
    file=${file//documentation/html}
    file=${file//.md/.html}

    echo "$html" > "$file"
}

recursive_html() {
    for entry in "$search_dir"/*
    do 
        # In case of folder iterate through it
        if [[ -d $entry ]]; then
            create_html_dict
            search_dir="$entry"
            entry="$entry"/index.md
            make_html
            entry=${entry//\/index.md/""}
            recursive_html
        # Else make an entry for the file
        elif [[ -f $entry ]]; then
            filename="$(basename -- $entry)"
            if [ $filename != index.md ]; then
                make_html
            fi
        fi
    done
}

rm -rf html
mkdir html

base_dir=$( cd "$(dirname "${BASH_SOURCE[0]}")" ; pwd -P )/
template=$(<templates/template.html)

cp templates/template.css html/
css_path="$base_dir"html/template.css

toc=""

search_dir="$base_dir"documentation
recursive_toc

search_dir="$base_dir"documentation
recursive_html

exit 0