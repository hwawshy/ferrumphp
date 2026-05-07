<?php
    ob_start();

    echo "Hi from PHP!";

    setcookie("langcookie", "cookiedata");
    header("Example-Test: foo");

    print_r($_POST);

    flush();
    ob_flush();

    print_r($_SERVER);