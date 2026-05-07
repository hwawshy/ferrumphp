<?php
    ob_start();

    echo "Hi from PHP!";

    setcookie("langcookie", "cookiedata");

    flush();
    ob_flush();

    header("Example-Test: foo");

    print_r($_SERVER);